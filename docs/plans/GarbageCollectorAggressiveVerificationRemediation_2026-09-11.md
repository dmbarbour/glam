# Aggressive GC Verification Remediation Plan — 2026-09-11

Status: GCI11R-002A-C complete; GCI11R-002D-H planned. This plan expands
GCI11R-002 and Phase I11D.1. The private repository mode exists and is useful,
but its complete workspace suite does not yet pass. Gate G3 remains closed.

## Purpose

`aggressive-gc-verification` forces a full collection before each eligible
outer entry into a production runtime value domain. Its purpose is to turn a
regional ownership gap into a deterministic failure at, or immediately after,
the former allocation/publication boundary. It is not a production collection
policy, a performance mode, or a reason to weaken semantic tests.

The initial repository run found three different kinds of problem:

1. production machine or compiler state carrying a bare managed edge across a
   boundary where neither a mutator region nor a traced registered owner
   remains;
2. test-only constructors deliberately opening their own region and returning
   a bare managed facade which a later statement attempts to root; and
3. deterministic collection fixtures whose one-shot probes or exact epoch
   assumptions are accidentally consumed by the additional verification
   collection.

This plan separates those workstreams. A fixture correction must not conceal a
production defect, and a schedule accommodation must not weaken the ordering
that the test exists to prove.

## Existing Mode and Preserved Semantics

The root feature forwards `glam-gc/deterministic-test-hooks` and enables
collection only after `EvaluationRuntime` construction has completed. The
collector skips a verification collection when the current thread is already
inside another heap; that request remains pending until an eligible outer
entry. The heap's immutable policy remains `CollectionPolicy::NoAuto`.

These properties are retained:

- the feature is private verification configuration, not supported embedding
  API;
- recursive entry into the same heap never initiates collection;
- nested entry into another heap never creates a cross-heap collection wait;
- an added verification collection changes only collector-operational state;
- semantic results and ownership expectations are identical in ordinary and
  aggressive runs; and
- no semantic or concurrency test is skipped merely because the feature is
  enabled.

## Audit Evidence

### Mode validity

The focused
`repository_aggressive_mode_enables_each_production_runtime` fixture passes.
It observes exactly one automatic collection before an eligible outer entry
and confirms that policy remains `NoAuto`. The previously repaired
`compiler_cache_does_not_form_a_value_domain_cycle` fixture also passes under
the feature: a closed cache family now builds beneath one outer managed region
until its declared roots have been installed.

### Representative exact failures

Each of the following passes in the ordinary configuration and fails in a
fresh aggressive process:

| Exact test | Aggressive observation | Initial classification |
| --- | --- | --- |
| `api::tests::access_and_annotation_construction_do_not_demand_inputs` | collector rejects a non-slot managed edge | invalid test fixture handoff |
| `api::tests::cached_defined_selection_helpers_match_glam_undefined_semantics` | evaluator dereferences an unallocated `ManagedLazyCell` | production poll-spanning owner defect |
| `reflection::machine::tests::reflection_environment_is_available_as_plain_data` | collector reaches an unallocated edge while projecting compiler values | production closed-evaluation result handoff |

The exact reproducer shape is:

```sh
cargo test -q -p glam --features aggressive-gc-verification --lib \
  TEST_PATH -- --exact --nocapture
```

The ordinary control omits `--features aggressive-gc-verification`. A passing
ordinary control shows that the mode has exposed a latent lifetime gap; it does
not by itself prove which owner should close that gap.

### Production machine-state gap

`EvaluatorStepContext` temporarily roots a newly constructed lazy until
`finish` has published the poll result or durable blocked state. One machine
state currently defeats that protection:

```text
LazyTaskWork::Follow(Value)
```

On a yielded poll, the raw deferred `Value` is stored in the Rust machine. The
step then drops its temporary publication roots. The owning lazy cell has not
yet cached the intermediate result, so its root does not trace this new follow
target. A later poll attempts to follow the now-unowned managed edge. The
defined-selection test reaches this case deterministically.

The intended owner is the poll-spanning machine state. It must retain a
`RuntimeValueRoot` (or an equally explicit exact managed root), installed
before the current evaluator step releases its temporary publication. The
next poll projects the value only under its admitted access region.

`PromiseFollowerState::FollowAssignment(Value)` looks similar but has a
different owner chronology. The same machine retains `ManagedPromiseRoot`, and
the immutable promise assignment traces the followed value for the entire
state. That field should receive a focused aggressive test and an inventory
record for its deliberate indirect owner; it does not justify a redundant
parallel root unless implementation reveals that the assignment can be
detached or replaced.

This is not a `Values::apply` construction defect: `ScopedValues::apply`
allocates the application lazy and publishes its outer public root in one
access region. The defect occurs after evaluation produces another deferred
value and yields while following it.

### Closed compiler-evaluation gap

`g_syntax::compiler_values::evaluate_closed` admits a rooted client demand,
then projects its terminal `RuntimeValueRoot` to a raw compatibility `Value`
and returns it. The completion root is gone before an outside caller opens a
new region to install the result. `ModuleLowerer::new` exposes the gap by
calling `reflection_annotator_value` and only afterward constructing its
`RuntimeValueRoot`.

Aggressive collection on that later entry reclaims the unowned result. A
subsequent collection follows the newly installed outer shell to the stale
edge, which explains why the reported failure may occur during a later cache
projection rather than at the original return.

The compiler needs a rooted closed-evaluation result boundary. Helpers may
either retain that result as a root, or project it inside a caller-owned
access region which installs it immediately beneath the intended definitions,
module, diagnostic, or cache owner. A helper returning a fresh bare managed
identity from `evaluate_closed` is not a valid durable API.

### Test-fixture regional gaps

The post-I5 migration deliberately retained a small set of `#[cfg(test)]`
self-opening constructors for isolated family tests. They are unsafe as
general fixtures in an aggressive production runtime. A representative API
test currently performs:

```text
LazyValue::semantic_thunk(&core_values, ...)
-- first access region ends; no root exists
public_value(&core_values, Value::Lazy(...))
-- a later access attempts to publish the outer root
```

The same shape is common in `src/evaluation/tests.rs` with
`PromisedValue::new`, `LazyValue::semantic_thunk`, and a later
`RuntimeValueRoot::new`. A source search currently finds 21 uses of the API
test `public_value` helper and dozens of self-opening lazy/promise constructors
across API, evaluation, coordinator, and reflection-store tests. Not every
occurrence is invalid: cached or already-rooted values may be projected safely.
The inventory must classify the owner chronology, not mechanically replace
every textual match.

General semantic fixtures should construct the managed identity and its real
fixture owner in one `RuntimeValueAccess`. A temporary root created only to
bridge two adjacent statements is not the target design. Self-opening helpers
may remain only in isolated collector-family tests whose explicit subject is a
different lifecycle contract and which do not claim repository aggressive
coverage.

### Schedule-fixture interference

`external_request_during_finalization_is_coalesced` currently installs its
one-shot Finalizing probe and then calls `runtime.values().unit()` while
journaling output. Under aggressive mode that value entry initiates collection
on the main thread, consumes the probe, and waits before the intended collector
thread has even been spawned. This is a deterministic harness deadlock, not a
collector synchronization defect.

All managed value setup must finish before a one-shot phase probe is installed.
Exact epoch assertions must snapshot the completed epoch immediately before
the collection whose ordering they prove. Additional setup collections may be
acknowledged only outside the disputed interval. Bounded waits remain harness
watchdogs; repetition and elapsed time are not ordering evidence.

### Failure clustering

The complete aggressive run reports failures across API, evaluation,
`g_syntax`, macro expansion, reflection machine, and reflection store tests.
Many reflection failures compile `.g` input and may therefore share the
closed-evaluation defect. Many evaluation failures use the same test-only
promise/lazy construction style. Aggregate unwinding can obscure the first
invalid edge and has previously ended in a secondary stack overflow, so the
remediation must use fresh exact processes and rerun each cluster after a
shared fix rather than assigning every current failing test an independent
root cause.

### GCI11R-002A failure matrix

The initial matrix uses a fresh test process for each named row. “Pass” is a
control which narrows the failing boundary; it is not evidence that the whole
subsystem is closed.

| Subsystem | Exact fixture | Aggressive result | Disposition |
| --- | --- | --- | --- |
| feature/policy | `repository_aggressive_mode_enables_each_production_runtime` | pass | mode enabled; `NoAuto` preserved |
| closed runtime cache | `compiler_cache_does_not_form_a_value_domain_cycle` | pass | prior shared cache-build repair remains valid |
| API fixture | `access_and_annotation_construction_do_not_demand_inputs` | fail from rooted `ManagedValueNode` edge | invalid fixture allocation/publication split; GCI11R-002E |
| evaluator machine | `cached_defined_selection_helpers_match_glam_undefined_semantics` | fail on unallocated `ManagedLazyCell` access | confirmed `LazyTaskWork::Follow` owner defect; GCI11R-002B |
| client-demand control | `client_demand_completes_whnf_into_its_result_cell` | pass | client-demand rooting is not categorically broken |
| evaluation fixture | `synchronous_whnf_facade_preserves_retryable_promise_behavior` | fail from rooted `ManagedValueNode` edge | self-opening promise fixture; GCI11R-002E |
| reflection store control | `snapshot_journal_edits_and_protected_volumes_retain_roots_without_forcing` | pass | isolated non-production factory does not enable repository mode |
| reflection store fixture | `query_result_remains_rooted_after_store_and_handle_retirement` | fail from rooted `ManagedValueNode` edge | production-runtime `unforced_store_value` fixture splits lazy allocation/publication; GCI11R-002E |
| macro control | `macro_runner_distinguishes_a_non_effect_value` | pass | macro runner is not categorically broken |
| macro/source integration | `inline_macro_readers_and_writers_are_transactional` | fail from rooted `ManagedValueNode` edge | consistent with closed compiler-evaluation cascade; confirm after GCI11R-002C |
| source/reflection | `reflection_environment_is_available_as_plain_data` | fail from rooted `ManagedValueNode` edge | confirmed closed-evaluation result handoff; GCI11R-002C |
| collection schedule | `external_request_during_finalization_is_coalesced` | setup can consume its own probe before spawning collector | deterministic schedule-fixture interference; GCI11R-002F |

The collector now enriches lookup panics with the immediate traced
predecessor's canonical Rust type and address plus the reported edge address.
A registered-root lookup failure similarly names its one-based retained-root
ordinal and address. This adds no ancestry allocation, root metadata, or
success-path lookup: it formats information already held by the failing mark
operation. The collector's invalid-edge fixtures assert that the primary
classification remains stable and that traced-edge failures name
`InvalidEdgeHolder` as their predecessor.

## Remediation Invariants

1. Every fresh managed edge leaves its allocation region only beneath its
   intended registered root or an already traced managed owner.
2. Any Rust machine or coordinator state that survives a poll owns its Glam
   values through direct runtime roots or through an immutable traced owner
   whose registered root demonstrably spans the same state. A bare
   compatibility `Value` is not ownership by itself.
3. Projection from a root is bounded by matching runtime access. A projected
   value cannot be returned across that access unless another intended owner
   has already been installed.
4. Closed evaluation returns an owned result. Raw projection is a caller-local
   operation, not the ownership-transfer representation.
5. Aggressive verification remains observationally irrelevant to Glam
   semantics and immutable heap policy.
6. One-shot probes are installed after unrelated managed setup and observe the
   exact transition named by the fixture.
7. No test is accepted because it passed under repetition. Nondeterministic
   orderings require barriers, probes, or another constructive schedule proof.
8. The I6+ Regional Allocation Migration Rule remains the construction rule
   for any new family or fixture helper introduced by this remediation.

## Checkpoints

### GCI11R-002A — Failure Matrix and Attribution Support

Status: complete on 2026-09-11.

1. Record ordinary and aggressive outcomes for one fresh-process reproducer
   per failing subsystem. Start with the three exact tests above.
2. Partition the full aggressive suite by API/runtime, evaluator/coordinator,
   `g_syntax`/macros, and reflection/store so one abort cannot hide all later
   results.
3. Add the smallest failure-only attribution aid needed when a collector
   lookup failure lacks its owning path, and validate it with deterministic
   tests. Prefer a root/worklist predecessor and managed-family description,
   or tightly placed forced collection checkpoints. Do not add production
   tracing cost merely to make panic text richer.
4. Maintain a failure matrix with one of four dispositions: production owner
   defect, invalid fixture handoff, schedule-fixture interference, or cascade
   from an already identified shared defect. “Transient” is not a disposition.

Exit: every stable aggressive failure cluster has an exact reproducer and an
initial evidence-backed owner classification. Attribution instrumentation is
failure-only and independently covered.

The matrix above partitions the stable failures and controls observed so far.
Immediate-predecessor attribution is implemented directly on the collector's
failure path because it adds no successful-trace state or production runtime
cost; it is nevertheless verified by deterministic invalid-edge fixtures.
The richer messages corroborate that the API fixture and compiler result both
installed rooted `ManagedValueNode` shells containing already stale inner
edges. Aggregate subsystem rows remain assigned to B-G and must be rerun after
each shared fix rather than treated as independent defects.

### GCI11R-002B — Poll-Spanning Evaluator Ownership

Status: complete on 2026-09-11.

1. First latch the current failure for a lazy result followed across two poll
   steps.
2. Replace bare `LazyTaskWork::Follow(Value)` with a canonical same-runtime
   `RuntimeValueRoot`. Install that owner during the producing evaluator step,
   before pending publications are retired, and project it only inside the
   consuming evaluator step. Do not retain the original `Value` beside it.
3. Replace `PromiseFollowerState::FollowAssignment(Value)` with a payload-free
   phase marker. The existing `ManagedPromiseRoot` is the one durable owner;
   every following poll reprojects its immutable assignment through matching
   value access. Add a focused fixture proving that this indirect ownership
   survives collection without a redundant assignment root or poll-spanning
   bare value.
4. Inventory all production evaluator, net-driver, reflection, and effect
   machine fields which retain `Value`, `Vec<Value>`, or a value-bearing shell
   across `Yielded` or `Blocked`. Durable state must use one canonical runtime
   root, an existing specialized managed root, or edge-free phase/identity
   data. Migrate any bare pointer-bearing value instead of adding a parallel
   root record; do not infer safety merely from a short Rust lifetime.
5. Update the machine/root source inventories so another poll-spanning bare
   edge is a review-visible change.

Verification:

- exact lazy-follow and promise-follow tests in ordinary and aggressive modes;
- `cached_defined_selection_helpers_match_glam_undefined_semantics` in both
  modes;
- focused evaluator/client-demand/promise suites; and
- source-latch tests for poll-spanning machine ownership.

Exit: no evaluator machine relies on step-temporary publication after that
step returns, and no repaired machine stores the same semantic state both as a
bare value and as a parallel root record.

#### Poll-spanning ownership and moving-GC retirement policy

Parallel roots are compatibility scaffolding, not an accepted target
representation. Across an evaluator safepoint, a root or managed owner is
canonical and any bare semantic view is projected afresh inside the next
mutator-qualified step. A companion root must not be added merely to preserve
a separately retained `Value`.

The later Value Representation Refinement removes the remaining representation
scaffolding: its structural-node conversion replaces the managed wrapper which
currently contains a monolithic compatibility `Value`, and its failure
conversion replaces `RuntimeFailureRoot`'s `Arc<EvaluationFailure>` plus direct
value roots with one managed/rooted failure representation. The root boundary
may remain; the duplicated semantic representation may not.

Moving collection is blocked until a source-backed retirement gate proves:

- no durable machine field contains a bare pointer-bearing `Value` across a
  yield, block, callback, or wait;
- no owner stores semantic data plus parallel roots for that same data;
- `CompatibilityValueEdges` has no remaining implementation; and
- every persistent managed edge is rewritable or uses an explicitly selected
  stable indirection, while registered root cells are the external relocation
  points.

The implementation now follows that policy. `LazyTaskWork::Follow` owns one
canonical `RuntimeValueRoot` installed before evaluator-step temporary owners
retire, while `PromiseFollowerState::FollowAssignment` carries no value and
reprojects the immutable assignment from its existing `ManagedPromiseRoot` on
every poll.

The production machine audit closed as follows:

| Machine family | Durable poll-spanning semantic ownership | Raw value policy |
| --- | --- | --- |
| lazy producer/follower | `ManagedLazyRoot` plus a canonical follow `RuntimeValueRoot` | projected only in the active evaluator step |
| promise follower | one `ManagedPromiseRoot` plus an edge-free phase marker | immutable assignment reprojected on every poll |
| net driver/normalizer | `ManagedCoreNetRoot`, ports, IDs, and observations | stack-bound claims and batch results are consumed in one drive |
| net construction | `IsolatedEffectSearch` and its public rooted branch/journal values | constructor input is wrapped before the pollable machine escapes |
| reflection/effect machine | compile-exhaustive `RuntimeValueRoot`, managed-promise-root, and rooted-failure fields | raw decoded requests and fused actions remain stack-bound |
| coordinator/client demand/spark | existing `RuntimeValueRoot` and `RuntimeFailureRoot` terminal or queued records | projection occurs only through evaluator or host access scopes |

`poll_spanning_evaluator_state_uses_canonical_owners` is the compile-exhaustive
evaluator latch. The exhaustive durable-owner scan now records
`LazyTaskWork` as the evaluator-machine owner and the existing reflection
`outer_machine_root_inventory_is_complete` latch covers its complete frames.
The focused lazy fixture failed deterministically before the repair under
aggressive verification and passes in both modes afterward; the new promise
fixture passes in both modes without an assignment companion root. The cached
defined-selection regression also passes in both modes. Broader aggressive
client-demand and task-promise filters still contain self-opening test
constructors assigned to GCI11R-002E; they are not evidence against this
production machine-state repair and remain required for cluster closure.

### GCI11R-002C — Rooted Closed-Evaluation Results

Status: complete on 2026-09-11.

1. Add a focused failure which evaluates a closed expression to a managed
   deferred identity, forces collection at the return/publication gap, and
   then attempts to use it. Preserve an immediate-result control.
2. Introduce an owned internal closed-evaluation result boundary, preferably a
   `RuntimeValueRoot` returned directly from client demand before projection.
   Do not add a root after the current raw result has already escaped.
3. Inventory every `evaluate_closed` call. Classify whether its result becomes
   a cache root, module/definition root, diagnostic value, macro value, or an
   immediate composition input.
4. Migrate returning helpers to one of two valid forms:
   - return/retain the owned result; or
   - accept caller-owned access and project the root only while installing the
     result in its final traced owner.
5. Remove or narrow any raw-returning closed-evaluation helper which could
   produce a managed identity. Update the `g_syntax` access inventory.

Verification:

- the focused return/publication-gap test;
- `reflection_environment_is_available_as_plain_data` in ordinary and
  aggressive modes;
- compiler-cache, module lowering, diagnostic formatting, macro expansion,
  and source compilation suites; and
- collection between closed evaluation and every formerly separate root
  publication represented by the inventory.

Exit: the compiler never transports an unowned managed result through a raw
`Value` return boundary.

The former gap was latched directly by
`closed_evaluation_result_is_owned_across_return_publication`. Before the
repair, a closed identity function was returned as a raw `Value`, an explicit
collection reclaimed its managed core net, and publication followed by the
next trace failed on that stale edge. A small integer survived the identical
interval as the allocation-free control. The repaired fixture passes in both
ordinary and aggressive modes.

`EvalContext::evaluate_root_whnf` now exposes the already-authoritative
`ClientDemandResult::Complete(RuntimeValueRoot)` to internal callers without
projecting it. `compiler_values::evaluate_closed` accepts and returns runtime
roots at both ends, so client-demand retirement and caller publication are one
continuous ownership chain. The ordinary raw-returning `evaluate_whnf` facade
is retained only for callers which consume its projection immediately; it is
not the compiler cache boundary.

The complete `evaluate_closed` call inventory has these dispositions:

| Closed-result family | Final ownership |
| --- | --- |
| initial compiler helper bundle | each builder returns its client-demand root directly into `GCompilerValues` |
| dynamic effect-path helper | rooted result is inserted directly into the runtime-local effect cache |
| built-in module definitions | applied constant-definition result becomes `RootedBuiltinModule::definitions` directly |
| module reflection annotator | returned root becomes `ModuleLowerer::module_reflection` without rerooting |
| imported constant definitions | a local result root remains live while its projection is installed into the declaration's traced definitions under the existing access region |
| macro environment | returned root crosses into `run_macro_effect`, which converts that same owner into the public macro value rather than wrapping a raw result |
| default diagnostic formatter | returned root becomes `CachedDiagnosticFormatter` directly |
| immediate compiler composition and tests | a local root remains in scope while its bounded projection is embedded or inspected |

The `g_syntax` source inventory now latches both the root count changes and the
owned `evaluate_closed -> evaluate_root_whnf` signature. The only
raw-projecting reflection-annotator helper is `#[cfg(test)]` and uses the
isolated compiler test factory; production module lowering uses the rooted
form. General production-runtime fixture migration remains assigned to
GCI11R-002E rather than weakening this boundary.

Verified during this checkpoint:

- the focused return/publication fixture in ordinary and aggressive modes;
- all compiler-value and diagnostic-formatter tests in both modes;
- `reflection_environment_is_available_as_plain_data` in both modes;
- `inline_macro_readers_and_writers_are_transactional` in both modes; and
- the compiler root/projection source inventory.

A broader aggressive `g_syntax::` partition completed 434 of 448 tests. Its
remaining 14 failures are reflection/macro construction fixtures or
representation comparisons already assigned to the production sweep and
fixture migration in D-E; neither focused C reproducer remains among them.
This partition is diagnostic evidence, not closure of those later checkpoints.

### GCI11R-002D — Production Runtime Root Sweep

After B and C remove the two known shared defects, rerun the exact failure
matrix. Begin with the evaluation-boundary cleanup below, then audit only
remaining production-shaped failures.

#### GCI11R-002D.1 — Regional Values and Rooted Orchestration

Preserve the following ownership distinction explicitly:

- public `api::Value` is already a durable runtime root;
- raw `core::Value` is regional and may exist only under
  `RuntimeValueAccess` or inside a durable owner whose trace reports its
  managed edges;
- `EvalContext` orchestrates work across polls, waits, worker handoff, and
  reflection or host boundaries, and therefore must not retain an active
  `RuntimeValueAccess`; and
- `EvaluatorStepContext` / `EvaluationValueAccess` is the scope for operating
  on raw values during one callback-free evaluator quantum.

Make `EvalContext::evaluate_root_whnf(RuntimeValueRoot)` the ordinary
orchestration entry. Inventory callers of the raw
`EvalContext::evaluate_whnf(&core::Value)` compatibility facade and:

1. migrate callers which already hold `api::Value` or `RuntimeValueRoot` to
   clone/reuse that registered root rather than project, allocate a containing
   `ManagedValueNode`, and register another root;
2. preserve the `ClientDemandResult::Complete(RuntimeValueRoot)` directly when
   the result becomes a public value, cache entry, or another rooted owner,
   rather than projecting and re-rooting it;
3. keep raw input or output only inside a bounded access region or while
   installing it into its real traced owner; and
4. remove, rename, or sharply narrow the raw compatibility facade so its
   ownership transfer and allocation cost cannot be mistaken for the normal
   path.

The first migration targets are `ValueEvaluator`, the reflection inspectors,
module sealing, and any other source-inventoried caller that begins with an
existing runtime root. Add a focused counter/inventory fixture proving that
such evaluation neither registers a replacement input root nor wraps the
completed client-demand root a second time. Root cloning is allowed: it shares
the existing `RootCell` and does not add a collector registration.

Passing `RuntimeValueAccess` through the complete synchronous-looking WHNF
driver is not a valid shortcut. The driver may suspend or invoke integration,
so an initial raw value must be transferred to durable ownership before its
access region closes. Today `ClientDemandOperation` obtains that ownership
through a `RuntimeValueRoot`. Eliminating even this one per-demand root later
requires moving client-demand state into a collector-traced owner (for example
a managed machine state or a root-adjacent trace-immediate frame) and installing
the input there while the initiating access remains active. That performance
transition is not a condition for closing the aggressive-verification defect;
this checkpoint must leave the ownership seam explicit and must not hold a
mutator across orchestration.

#### GCI11R-002D.2 — Remaining Production Owners

Audit:

1. public value composition and evaluation-result publication;
2. task, client-demand, spark, wait, failure, and event/output records;
3. interaction-net and reflection-machine yielded/blocked state;
4. module, definitions, compiler cache, macro, and diagnostic ownership; and
5. reflection-store edits and query responses.

For each failure, latch the former gap, repair it through its real durable
owner, and update the authoritative source inventory. Split this checkpoint by
subsystem if more than one independent production representation changes.

Exit: already-rooted orchestration performs no project/re-root round trip, the
remaining raw facade has an explicit regional justification at every caller,
and all production-shaped exact tests pass aggressively before general test
fixtures are migrated. This ordering prevents a fixture helper from hiding a
runtime defect.

### GCI11R-002E — Test Fixture Regional Migration

1. Build a source-backed inventory of self-opening `#[cfg(test)]` constructors
   reachable from a production `EvaluationRuntime` under aggressive mode.
2. Add narrow fixture builders which allocate and install the actual fixture
   owner in one access region. Return the durable root/public value plus any
   facade projected from that owner. Do not preserve a bare edge merely for
   ergonomic parity with the old helpers.
3. Migrate API fixtures, beginning with the 21 `public_value` call sites. Keep
   already-rooted compiler/cache projections distinct from genuinely fresh
   managed identities.
4. Migrate `SameRuntimeFixture` and related evaluator/coordinator promise,
   lazy, and net fixtures. Preserve the intended owner: resolver, task record,
   public value, or explicit runtime root.
5. Migrate reflection-store fixtures and any remaining production-runtime
   users.
6. Retain self-opening family helpers only in explicitly inventoried isolated
   tests where aggressive production-runtime entry is not the subject. Add a
   source latch preventing their return to general runtime fixtures.

Verification:

- `access_and_annotation_construction_do_not_demand_inputs` in both modes;
- API, evaluation, coordinator, and reflection-store test partitions in both
  modes; and
- a forced collection at each representative former constructor/root gap.

Exit: general runtime tests follow the same regional publication contract as
production code.

### GCI11R-002F — Schedule-Fixture Adaptation

1. Inventory every one-shot collector probe and all managed entries between
   probe installation and the intended collection action.
2. Reorder
   `external_request_during_finalization_is_coalesced`: construct and commit
   output state first, install the Finalizing probe last, then start the
   intended collector.
3. Snapshot exact completed epochs immediately before disputed collections.
   Keep exact `+ 1` assertions within the latched interval; use a documented
   inequality only for unrelated setup which aggressive mode may extend.
4. Run admission-wait, Finalizing, request-coalescing, passive-finalization,
   and RAII probe tests in both modes. Prove both sides of each ordering with
   the existing probe/barrier rather than a sleep or repeated run.

Exit: aggressive collection cannot steal a test's one-shot probe, and every
schedule claim remains constructively ordered.

### GCI11R-002G — Cluster Closure

Run each subsystem in a fresh process after the shared repairs:

1. API and production runtime;
2. evaluator, coordinator, client demand, tasks, promises, and sparks;
3. `g_syntax`, module lowering, macros, and diagnostic formatting;
4. reflection machine, lifecycle, requests, and store; and
5. interaction-net and collector integration fixtures.

Any remaining failure receives an exact test and a disposition in the matrix.
Do not proceed while an aggregate panic, hang, or stack overflow prevents a
cluster from completing.

### GCI11R-002H — Repository Certification

Run and record:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
cargo test --workspace -q
cargo test --workspace -q --features aggressive-gc-verification
```

Also run the focused ordinary/aggressive schedule matrix and every source
inventory changed by B-E. A serial aggressive run is useful for detecting
hidden test coupling, but is not evidence for a concurrent ordering; the
latched fixtures provide that proof.

Before closure, verify:

- no test is feature-skipped to make the suite green;
- the mode still applies only to complete production runtimes;
- `NoAuto` remains immutable;
- the ordinary suite remains unchanged semantically;
- every former failure has a recorded resolution; and
- the GCI11R-002 review text and I11D.1 status link to the completed evidence.

Exit: both complete repository modes pass, GCI11R-002 is resolved, and I11D.1
is complete. Only then proceed to I11D.2 dynamic unsafe-boundary verification.

## Risk and Partitioning Notes

GCI11R-002B and C are independent ownership repairs and should remain separate
commits. D is an audit gate, not permission for an unbounded refactor; split it
as soon as it identifies more than one representation change. E is largely
mechanical but broad, so partition it by fixture family. F is independent
schedule work and may run after the production owner fixes without waiting for
all fixture migration.

The attribution aid in A is disposable verification machinery unless it
proves useful to later collector work. Do not compensate for a difficult
ownership diagnosis by adding heap/domain tokens to every `Gc<T>` or by
turning release-build pointer access into a global allocation lookup.

## Deferred Work

- production automatic collection policy and threshold servicing remain I12;
- dynamic Miri/sanitizer verification remains I11D.2;
- moving and concurrent collection remain their own plans;
- lifetime-branded managed pointers remain a possible later safety aid; and
- performance tuning of compatibility tracing waits until correctness and
  aggressive repository certification are complete.
