# Architecture, Separation of Concerns, and Verification Review — 2026-10-03

Reviewed revision: `7452e13c0730fcace7b39ef30e6d4176745945e4`.
The worktree was clean at the start. This review changes documentation only.

## Assessment

The architecture is a sound basis for further work, but the current checkout
does not yet have a clean verification baseline. Two concrete lifecycle/I/O
defects and several verification gaps should be addressed before treating it
as the reference implementation for performance changes.

The important existing boundaries are explicit and worth preserving: syntax
ends at resolution/lowering; evaluation consumes closed semantic values;
generic interaction nets own topology; reflection owns effects and host
callbacks; the executable owns configuration, rendering, and exit policy.
Runtime-qualified roots and bounded access are substantial improvements over
implicit ownership. This review does not recommend replacing these boundaries
or reopening the completed collector transition.

Priorities below mean P1 = a concrete defect with serious consequences,
P2 = a correctness/verification gap to close in preparation for optimization,
and P3 = a maintainability improvement that can be scheduled independently.
Source-only conclusions are identified separately from reproduced behavior.

## Scope and Method

Read the required design/context/module-map documents, the holistic design
references, all five current architecture documents, relevant invariant notes,
and recent collector/WHNF completion reviews. Followed representative paths
through source loading, staged compilation, public value construction and
observation, demand ownership, executor activation, coordinator transitions,
reflection transactions, and collector admission. Inspected verification
scripts, syntax-backed inventories, and external embedding/sample tests.

This is a cross-subsystem code review, not a line-by-line recertification of
every unsafe operation. Historical completion reviews provide context;
current source and checks below provide the evidence for this report.
Miri, sanitizers, scale fixtures, and a complete aggressive-GC run were not
repeated here. Their dated results remain historical evidence.

## Findings

### AR-001 — P1: Manifest output can overwrite an input through an alias

Owner: [`source.rs`](../../src/source.rs),
`FileSourceSystem::write_manifest`, lines 445–472.

The input protection checks `observed.contains_key(&output)` using an absolute
path spelling, then calls `fs::write`. An existing output symlink pointing to
a tracked input has a different spelling, passes that check, and is followed
by `fs::write`, which truncates the input. An existing hard link presents the
same identity problem.

**Reproduced through the public API**, using a temporary Rust executable
linked against this checkout's built library. All files were under `/tmp`:

```rust
let input = dir.join("input.bin");
let alias = dir.join("manifest.txt");
std::fs::write(&input, b"review input bytes").unwrap();
std::os::unix::fs::symlink(&input, &alias).unwrap();
let sources = glam::FileSourceSystem::default();
glam::SourceSystem::load_top_level(&sources, &input).unwrap();
sources.write_manifest(&alias).unwrap();
assert_ne!(std::fs::read(&input).unwrap(), b"review input bytes");
```

The call succeeded and the original input contained manifest bytes afterward.
This is a source/output identity defect, independent of evaluator behavior.

**Recommendation:** give manifest publication an explicit filesystem identity
and replacement policy. Reject outputs resolving to tracked inputs and write
through a fresh sibling temporary file with atomic replacement rather than
truncating an existing destination. Account for symlinked parent directories
and hard links when defining the policy; a lexical path check alone is
insufficient. Snapshot the observed digest map under its mutex, then perform
formatting and output I/O after release. Retain the consumed digests rather
than deriving manifest hashes from a later read.

**Verification:** ordinary output, exact input path, output symlink, input
symlink, parent-directory alias, hard-link alias, and failed publication.
Every rejected case must leave input bytes intact. This report exercised the
output-symlink case; the other cases are proposed regressions.

### AR-002 — P2: Worker activation is not transactional on spawn failure

Owner: [`evaluation/executor.rs`](../../src/evaluation/executor.rs),
`EvaluationExecutor::activate_workers`, lines 66–107;
[`api/runtime.rs`](../../src/api/runtime.rs), public `activate_workers`.

Activation sets `activated = true` before creating workers. Each successful
spawn immediately enters `evaluation_worker` and is added to `workers`.
The fallible spawn uses `?`, but the worker count and coordinator availability
are published only after the entire loop succeeds.

If worker 0 starts and worker 1 fails, public activation returns an error while
worker 0 remains live. `worker_threads()` still reports zero, the coordinator
has not received `executor_started`, and retry is rejected as already
activated. Even failure on the first spawn consumes the activation attempt.
The constructor path eventually drops a failed executor; the public activation
path can retain the inconsistent executor in a live runtime.

**Evidence:** source ordering establishes the failure path. No OS thread
resource exhaustion was induced, and no deterministic spawn-failure injection
exists in the inspected executor tests. The current tests cover successful
activation, duplicate activation, idle shutdown, and cache retirement.

**Recommendation:** prepare workers behind a startup gate, then publish the
complete activation before releasing them to claim work. On preparation
failure, retire the prepared workers and leave a documented retryable or
terminal activation state. Keep activation/shutdown authority in the executor;
do not repair this with a second scheduler or ad hoc worker-count sampling.

**Verification:** a private injectable spawner should fail deterministically
on the first and a later spawn. Check live thread retirement, reported count,
coordinator availability, retry semantics, and that no work executes before
successful publication.

### AR-003 — P2: The current baseline fails both tests and strict Clippy

Owners: [`api/runtime/gc_activity_inventory.rs`](../../src/api/runtime/gc_activity_inventory.rs),
lines 287–300; [`runtime.rs`](../../src/runtime.rs), lines 36 and 932;
[`reflection/store.rs`](../../src/reflection/store.rs), line 114.

The ordinary library suite fails
`runtime_gc_policy_review_links_selected_plan_and_completion_gate` at line 299.
It requires the historical wording `not enabled. Collector stress`, while the
roadmap now says `not enabled by deliberate policy` and links the C8 review.
The implementation's `NoAuto` policy has not changed: this is a test coupled
to editorial prose, not evidence of a collection-policy defect. The focused
test independently reproduced the same failure.

Strict Clippy fails on three remaining `AtomicU64::fetch_update` calls under
installed Rust `1.99.0 (b940084d7 2026-09-28)`. They are deprecated in favor of
`try_update`. The root package and collector manifests have no `rust-version`,
and there is no checked-in toolchain file. A prior passing matrix under a
different compiler does not establish that this checkout passes today.

**Recommendation:** update the atomic calls while preserving checked ID
allocation semantics; decide and document the supported compiler baseline.
Replace the prose-fragment assertion with a stable policy/link contract. Keep
the source-backed check of the single `NoAuto` constructor and the behavioral
pressure/maintenance tests. Historical paragraphs should be editable without
changing a runtime test's expected wording.

The documentation-only exemption in
[`AgentContext.md`](../AgentContext.md) also needs a qualification: documents
included by Rust inventory tests are test inputs. Changes to those documents
need the affected inventory tests, even when no `.rs` file changes.

### AR-004 — P2: Routine commands omit the collector's own test suite

Owners: [`Cargo.toml`](../../Cargo.toml),
[`AGENTS.md`](../../AGENTS.md), [`AgentContext.md`](../AgentContext.md),
and [`glam-gc/scripts/check.sh`](../../crates/glam-gc/scripts/check.sh).

Cargo metadata confirms two workspace members but only the root `glam`
package in `workspace_default_members`. Consequently, the documented
`cargo test -q` does not run `glam-gc`'s unit tests, seven Loom models, or eight
doctests/compile-fail contracts. Linking the collector into Glam tests does
not run the collector's own tests. Similarly, root-only Clippy does not select
the collector's test/example targets for linting.

The collector has its own good verification script and dynamic-tool scripts,
but no single checked-in entry point composes the workspace baseline,
profiling fixtures, ownership audits, and aggressive verification. No
checked-in CI workflow was found. Feature compilation via Clippy also does
not execute feature-specific tests; the profiling script deliberately runs
only 21 selected fixtures.

**Recommendation:** provide one ordinary workspace verification command and a
separately named extended safety command. Use explicit package/workspace
selection, list the features actually executed, and make missing named
fixtures fail closed as the profiling script already does. The extended
command should retain aggressive-GC execution, unsafe audits, code-generation
checks, targeted scale checks, and documented dynamic-tool exceptions.
Pin/record toolchain identity and preserve results against the exact revision.
Choose CI or a documented local gate without requiring every edit to rerun
all expensive tools.

### AR-005 — P2: Raw-value safety still relies on review, not escape analysis

Owners: [`core/managed.rs`](../../src/core/managed.rs),
`with_runtime_value_access`, lines 588–606;
[`runtime.rs`](../../src/runtime.rs), `clone_core_with` / `with_core`;
[`core/managed/raw_value_api_inventory.rs`](../../src/core/managed/raw_value_api_inventory.rs),
especially disposition classification at lines 105–124.

The higher-ranked callback prevents the access carrier and managed borrows
from escaping. It does not brand an owned `core::Value`: the callback's
unconstrained result type can carry an owned shell containing managed edges.
The raw-value inventory explicitly acknowledges this limitation. It counts
and fingerprints signatures, and classifies a signature as regional when it
has an access carrier; it does not prove that the carrier is used correctly
or that the returned value is rooted before the region ends. Likewise,
declaration inventories cannot prove arbitrary local data flow or callback
placement.

**Disposition:** an existing, documented architectural constraint, not a newly
demonstrated escaping production value or collector soundness defect. The
completed ownership inventories and forced collection fixtures provide real
evidence. Their value should not be confused with a type-level guarantee.

**Recommendation:** make root-returning construction the default seam and
keep raw operations inside narrow semantic modules. For each optimization
that changes root publication, moves a callback, batches access, or retains a
checkpoint, identify its durable owner and force collection/suspension across
that exact boundary. Use signature inventories to require review, not as a
substitute for those behavioral tests. Reconsider branded regional working
values only within the existing deferred scoped-pointer work; a repository-
wide representation rewrite is not a prerequisite for ordinary optimization.

### AR-006 — P2: Sample success does not always exercise compilation

Owners: [`tests/sample_sources.rs`](../../tests/sample_sources.rs), lines
31–47; [`tests/invalid_samples.rs`](../../tests/invalid_samples.rs), lines
18–28 and `assert_expectations`;
[`g_source.rs`](../../src/g_source.rs), `deferred_macro_declarations`.

The positive fixture walker checks only errors from `inspect_g_source`.
That API deliberately returns `MacroDeferred` for macro-bearing declarations,
so a passing sample test does not establish that those declarations expand,
resolve, or lower successfully. Macro protocol and executable tests cover
specific useful cases, and staged/batch equivalence tests exist, but the
fixture walker has no mapping that establishes which deferred examples are
covered by which compilation tests.

The negative syntax walker also accepts an empty or comment-only `.expect`
file: it parses zero expectations and its assertion loop does no work. This
can silently turn an invalid fixture into a test that cannot fail. No existing
empty expectation file was demonstrated; this is a harness verification gap.

**Recommendation:** classify samples by inspection-only, compile-with-fixture-
environment, or executable expectation. Require every `MacroDeferred` sample
to have an explicit compilation owner or documented inspection-only status.
Reject empty negative expectation sets and require an error expectation for
fixtures designated invalid syntax. Keep representative differential tests
between staged/batch lowering and fused/unfused interpretation when changing
those paths. Do not force abstract/design examples to execute without their
required environment.

### AR-007 — P3: Public lifecycle guidance is buried in internal architecture

Owners: [`lib.rs`](../../src/lib.rs),
[`api/value.rs`](../../src/api/value.rs), public `Value` / `Values`;
[`api/assembly.rs`](../../src/api/assembly.rs), `drain_reasoning`;
[`README.md`](../../README.md).

The internal evaluation guide clearly distinguishes the runtime facade,
value-domain lease, roots, weak observers, and external demand ownership.
The public `Value` documentation says it is rooted in one runtime but does
not itself explain that retaining the root does not retain the value domain.
`EvaluatedValue` does document failed extraction after domain loss. The
`drain_reasoning` documentation describes settlement/deadlock but omits its
possible maintenance-required/maintenance-failed results. The public test
helper `settle_ready_reasoning` panics on either maintenance result rather
than serving as a copyable client lifecycle example.

**Recommendation:** add a small embedding guide and compiling examples for
construction, outer-WHNF demand, owned extraction, domain lifetime, and the
pump/readiness/maintenance/settlement loop. Describe retryable stale snapshots
and retained maintenance failures without exposing mutator machinery. Link
from the public facade and README. This can remain much shorter than the
850-line internal evaluation architecture guide.

Also make the built-in-only compiler limitation explicit at `ModuleInput`:
the script extension currently contributes a label, while
`build_module_inner` always calls `g_syntax::compile_source`. The target
compiler-selection/bootstrap design is not an implemented extension dispatch
contract. Keep that distinction visible in current assembly documentation.

## Refactoring Opportunities

### RF-001 — Separate semantic representation from value-domain services

[`core.rs`](../../src/core.rs) currently owns semantic types and builtins,
`RuntimeValueDomain`, factory/cache construction, lazy/promise coordination,
host-call owners, reflection computations, and diagnostic/debug projection.
It depends on evaluation contracts, while managed lazy storage and
[`core_net.rs`](../../src/core_net.rs) also depend on evaluator checkpoint
types. This is a consciously coupled runtime substrate, not a strict acyclic
layer beneath evaluation.

Start with private modules for value-domain/factory/cache services, host-call
and reflection ownership, and semantic representations. Preserve the existing
facade paths and keep compiler caches type-indexed; core must not acquire
syntax or compiler types. Document the permitted checkpoint/scheduler
dependencies rather than pretending all `core -> evaluation` references are
violations. Only introduce a shared protocol module if it removes a concrete
ownership ambiguity; avoid premature trait erasure or a multi-crate rewrite.

Acceptance: unchanged public behavior and root lifetime, exact ownership
inventories reconciled with explanations, no new domain-retaining cycles,
and the relevant collection/owner-drop fixtures passing.

### RF-002 — Keep coordinator mutation central while isolating route logic

[`evaluation/coordinator.rs`](../../src/evaluation/coordinator.rs) already
has lifecycle children, but also contains the exact-demand zipper validation,
rebuild and causal-help traversal (roughly lines 3012–3760), registry/index
maintenance, generation publication, and common closure policy.
`WorkCoordinatorState` maintains separate foreground and background
registries plus several session, wait, ready, and observation indexes.

Extract exact-route traversal into a private child that borrows coordinator
state under the existing lock. Put index updates and common transition
assertions behind small helpers, especially where admission/retirement must
update several maps together. Document legal state/machine combinations:
`Running` has a detached claimant, `Blocked` has an exact dependency/epoch,
and terminalization retains producer obligations until publication completes.
Add a test-only whole-state consistency checker where individual inventory
tests currently verify only source spellings.

Preserve distinct foreground execution policy, the single mutation authority,
post-lock notifications/destruction, and non-authoritative route hints. A
generic scheduler rewrite or session-wide execution lane would obscure the
existing causal and ownership contracts.

### RF-003 — Make the verification machinery easier to maintain

The source-backed inventory modules contain repeated filesystem walking,
`cfg(test)` handling, alias/import analysis, fingerprints, and transition-era
owner naming. Some coordinator inventories also require exact whitespace.
This creates additional work for otherwise mechanical refactors and can hide
whether a failure means semantic drift or an editorial/source-format change.

Share only the test-only source traversal and parsing utilities first. Keep
each invariant's policy and expected inventory local and independently
reviewable. Preserve fail-closed discovery and parser self-tests. Move prose
and link contracts into a focused documentation check, normalize Markdown
structure where appropriate, and keep behavioral tests authoritative for
publication order. Do not make updating a fingerprint sufficient evidence
that a new edge or callback is safe.

## Boundaries and Evidence to Preserve

| Boundary | Current evidence and implication |
| --- | --- |
| Syntax to semantics | Affine `ResolvedExpr<Value>` lowers directly to closed values/nets; staged/batch tests check representative results and diagnostic order. Do not reintroduce syntax ASTs or local environments into evaluation. |
| Generic nets to Glam semantics | Generic topology/reduction is specialized through `core_net`; callable WHNF and managed checkpoints live in semantic owners. Optimize topology without importing syntax or host policy. |
| Durable roots to regional edges | Opaque public values, explicit rooted checkpoints, exhaustive payload visitors, and owner inventories establish the supported ownership graph. A root-elision change needs exact liveness evidence. |
| Demand lease to scheduler work | Coordinator records own machines; contexts cannot recover the owner lease. Keep cancellation/closure and stale subscription tests when changing scheduling. |
| Pure evaluation to callbacks | Bounded value access ends before reflection callbacks, output delivery, and diagnostics. Keep forced callback-placement and suspension tests when fusing or batching. |
| Transaction to external output | Reflection/event publication is guarded; outboxes deliver after locks release. Optimization must preserve branch isolation, per-endpoint order, and retained failure reporting. |
| Runtime pressure to collection | Production heaps remain immutable `NoAuto`; stable pumping promotes pressure and explicit revision-checked maintenance services it. Continuously busy runtimes may defer service by accepted policy. |

The existing differential fused/unfused fixtures, shared-budget tests,
barrier/channel lifecycle tests, Loom models, compile-fail contracts, and
profiling-only operation counts are useful foundations. A concurrency claim
requires a forced disputed ordering; repeated uncontrolled runs establish only
stress evidence. Optimizations should compare observable results, structured
failures, transaction effects, and suspension behavior, while measuring cost
separately from those contracts.

## Verification Performed

Environment: Rust `1.99.0`, Cargo `1.99.0`, Linux x86-64 workspace.

| Check | Result |
| --- | --- |
| `cargo fmt --check` | Passed. |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Failed: three deprecated atomic calls in Glam; see AR-003. |
| `cargo test -q --workspace` | Failed in the Glam library: 1,910 passed, one failed, two ignored, 195.98 seconds. The failure stops the remaining targets; see separate runs below. |
| Focused failing documentation inventory | Reproduced the line-299 prose assertion failure. |
| `cargo test -q --package glam-gc --all-features` | Passed: 214 unit tests, seven Loom tests, eight doctests; two scale tests ignored. |
| `scripts/check-interaction-net-profiling.sh` | Passed all 21 selected fixtures. |
| `crates/glam-gc/scripts/audit-unsafe.sh` | Passed ledger diffs and collector all-target/all-feature check. |
| Public manifest-alias reproduction | Confirmed successful publication through a symlink overwrites tracked input bytes. |
| Explicit Glam binary and eight integration suites | Passed: 81 binary unit tests and 114 integration tests across all eight suites. |
| Review links and `git diff --check` | Validated after writing this report. |

These results do not certify the omitted dynamic-tool, scale, release, or
complete aggressive/combined-feature matrix. An all-feature build and a
selected profiling run are not substitutes for executing that matrix.

## Recommended Sequence Before Performance Plans

1. Repair AR-001 and AR-002 with focused identity/failure-injection tests.
2. Restore the clean baseline in AR-003 and establish the explicit workspace
   verification entry points in AR-004. Record a fresh passing revision and
   compiler identity.
3. Close the sample harness gaps in AR-006 and add the short public lifecycle
   guide in AR-007. Keep AR-005's regional/root distinction as a required
   review criterion for every ownership-affecting optimization.
4. Take RF-001 through RF-003 in narrow, behavior-preserving slices where they
   reduce the scope of the intended performance change. They need not become
   an unrelated broad rewrite before profiling can begin.
5. Start performance planning from representative end-to-end workloads and
   retained semantic/differential oracles. Attribute measurements to the exact
   source revision, compiler, features, worker policy, and maintenance policy.

No language semantics or production policy were changed by this review.
The proposed atomic output, activation retry, and fixture classification
policies need explicit decisions in their follow-on fixes. The worker failure
conclusion is source-backed; deterministic injection remains its missing proof.
