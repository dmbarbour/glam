# Aggressive GC Verification Remediation Plan — 2026-09-11

Status: GCI11R-002A-C and D.1a-D.2g complete; GCI11R-002D.2h planned. This plan expands
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
- raw `core::Value` is regional: it may be stored outside an access region
  only as the unobserved payload of a durable owner whose trace reports its
  managed edges;
- every API which constructs, projects, inspects, clones, or transfers a
  `core::Value` must itself carry matching `RuntimeValueAccess` (directly or
  through `EvaluationValueAccess`); it may accept and return raw values while
  producer and consumer remain in that same access region, but must install a
  raw result into durable traced ownership before it crosses the region;
- `EvalContext` orchestrates work across polls, waits, worker handoff, and
  reflection or host boundaries, and therefore must not retain an active
  `RuntimeValueAccess`; and
- `EvaluatorStepContext` / `EvaluationValueAccess` is the scope for operating
  on raw values during one callback-free evaluator quantum.

##### GCI11R-002D.1a — Exhaustive Raw-Value API Audit

Do not infer closure from the known evaluation facade. The existence of
`EvalContext::evaluate_whnf(&core::Value)`, which accepts a raw value without
access authority and roots it internally, is the initial witness that this
invariant has not yet received a source-wide audit.

Inventory every production function, method, trait method, callback type, and
type alias whose parameters or return shape contain `core::Value`, including
qualified/imported spellings and wrappers such as references, `Option`,
`Result`, `Vec`, slices, arrays, `Arc<[Value]>`, and closure arguments or
results. Audit test-only APIs separately in GCI11R-002E rather than allowing
fixtures to define the production contract. Also inventory inherent and
standard-trait operations on `core::Value`: collector-exclusive `Trace` and
destruction may have a documented authority contract other than a mutator,
but `Clone`, equality, formatting, and similar representation operations must
not become an unexamined escape hatch. Merely proving that all current callers
happen to hold access is insufficient when the API shape does not carry that
authority. Fields which durably store raw values belong to D.2's traced-owner
audit, but every operation which reads or updates such a field remains in this
API inventory.

Classify each production occurrence as exactly one of:

1. **regional access API:** matching `RuntimeValueAccess` or
   `EvaluationValueAccess` is present and remains live while the raw value is
   produced and consumed;
2. **scoped exposure:** a higher-ranked/access-scoped callback may observe or
   return raw values only within the region opened by the API;
3. **durable boundary:** the public signature transports `api::Value`,
   `RuntimeValueRoot`, or another traced owner rather than raw `core::Value`;
4. **collector-mandated primitive:** an exact, narrow allowlist such as
   collector-exclusive `Trace` or destruction uses collector phase authority
   because a mutator is structurally unavailable; or
5. **violation:** the API can pass, return, project, inspect, clone, or retain a
   raw value without matching authority.

Record the inventory in the ownership ledger or a dedicated review table with
the signature, caller families, classification, and intended disposition.
First latch the known violations—including raw `evaluate_whnf`—and assign
their intended repairs to D.1b-D.2. Those repair checkpoints then change the
source-backed check from a pre-repair baseline into a closed gate. The latch
must detect container and alias forms rather than merely count the literal
text `-> Value`. If a robust syntax-aware gate is disproportionate, use a
conservative source inventory plus an exact reviewed allowlist; do not silently
omit signatures that the scanner cannot classify.

Exit for D.1a: every production signature transporting raw `core::Value` has
a current classification and intended disposition, known violations are
latched, and repository drift changes the check. D.1b-D.2 close those
violations and establish the final rule that every raw API has matching
mutator/access authority or a reviewed collector-only exception.

Completion record (2026-09-11): the
[raw core-value API audit](../reviews/GarbageCollectorRawValueApiAudit_2026-09-11.md)
records and source-latches 586 production declarations: 69 access-qualified
operations, 23 collector-only compatibility operations, eight regional alias
definitions, and 486 known violations. The initial raw `evaluate_whnf`
witness is explicitly latched. D.1a changes no production semantics; D.1b and
D.2 own the repairs and replace the violation baseline with a closed gate.

##### GCI11R-002D.1b — Rooted Orchestration and Regional Handoffs

Status: complete on 2026-09-12.

Make `EvalContext::evaluate_root_whnf(RuntimeValueRoot)` the ordinary
orchestration entry. Inventory callers of the raw
`EvalContext::evaluate_whnf(&core::Value)` compatibility facade and:

1. migrate callers which already hold `api::Value` or `RuntimeValueRoot` to
   clone/reuse that registered root rather than project, allocate a containing
   `ManagedValueNode`, and register another root;
2. preserve the `ClientDemandResult::Complete(RuntimeValueRoot)` directly when
   the result becomes a public value, cache entry, or another rooted owner,
   rather than projecting and re-rooting it;
3. require a matching access for every raw input, output, or handoff; permit a
   handoff to return a raw value to a caller which still holds that access, or
   install the value into its real traced owner before returning across the
   access boundary; and
4. remove, rename, or sharply narrow the raw compatibility facade so its
   ownership transfer and allocation cost cannot be mistaken for the normal
   path.

The first migration targets are `ValueEvaluator`, the reflection inspectors,
module sealing, and any other source-inventoried caller that begins with an
existing runtime root. Add a focused counter/inventory fixture proving that
such evaluation neither registers a replacement input root nor wraps the
completed client-demand root a second time. Root cloning is allowed: it shares
the existing `RootCell` and does not add a collector registration.

`RuntimeValueAccess` is appropriate for either side of a regional *handoff*.
An ordinary callback-free helper may return raw values when its caller remains
inside the same region and will inspect, install, or root them before releasing
the access. The current unbranded `core::Value` type does not encode that
lifetime, so the access-bearing API shape and source inventory must enforce the
contract for now.

The access must not remain live through the complete synchronous-looking WHNF
driver. The driver may suspend or invoke integration, so an initial raw value
must be installed into durable ownership while the initiating access remains
active; that access then closes before orchestration begins. Today
`ClientDemandOperation` obtains durable ownership through a
`RuntimeValueRoot`. On completion, an orchestration API may either return that
durable root or open a fresh bounded access and expose the projected raw result
only to an access-scoped consumer. It must not return the projection after that
fresh region has closed.

Eliminating even this one per-demand root later requires moving client-demand
state into a collector-traced owner, such as a managed machine state or the
root-adjacent trace-immediate `RootFrame` model recorded for concurrent GC. A
future evaluator access/frame may similarly replace
`pending_managed_publications`' bundle of temporary family roots: it would
trace its regional values as one durable frame between quantums, while every
read or mutation of those values still occurs under matching
`RuntimeValueAccess` and the relevant edge-transition protocol. This is an
ownership representation change, not permission for raw-value APIs to operate
without a mutator.

That performance transition is not a condition for closing the
aggressive-verification defect. This checkpoint must nevertheless leave the
handoff seam explicit, permit raw returns only within matching access
authority, prohibit unaccompanied raw-value APIs, and never hold a mutator
across orchestration.

Completion record (2026-09-12): public `ValueEvaluator` and reflection
inspection now pass the existing registered public root into
`evaluate_root_whnf`; module sealing similarly demands its existing
definitions root after publishing the final-definition promise. A monotonic
collector verification counter proves an immediate managed public input adds
only the one unavoidable client-demand completion root: there is no temporary
replacement input root, and cloning the returned public value adds no root
registration. The ambiguous raw `evaluate_whnf` name was removed. Remaining
raw diagnostic/front-end compatibility callers are explicitly named
`evaluate_compatibility_whnf`, source-latched as violations, and assigned to
D.2 rather than masquerading as the ordinary orchestration path.

#### GCI11R-002D.2 — Remaining Production Owners

Status: planned; this is a hard prerequisite for Gate G3.

D.2 closes both kinds of production violation which D.1a deliberately left
open:

1. an API can construct, project, inspect, clone, compare, format, pass, or
   return raw `core::Value` without carrying matching regional authority; or
2. a field, closure capture, alias, or other durable owner can retain a raw
   value without an exact traced/rooted ownership account.

Aggressive collection may expose examples of either defect, but test coverage
is not the closure mechanism. An unused authority-free API is still a
violation. The D.1a syntax inventory currently records 486 such production
operations, while the durable-owner inventories cover the complementary field
and capture surface. D.2 must reduce the operation count to zero and reconcile
the owner inventories before GCI11R-002E is allowed to make test fixtures look
green.

##### GCI11R-002D.2a — Closure Policy and Occurrence Assignment

Status: complete on 2026-09-12.

1. Turn D.1a's family summary into an occurrence-level remediation manifest.
   Every current violation names exactly one D.2b-D.2g owner and one intended
   replacement shape. A count/fingerprint remains the drift latch, but is not
   a substitute for this assignment.
2. Join the operation inventory with the durable field, closure-capture,
   machine-state, and external-owner inventories. Add a syntax-backed field or
   carrier scan wherever the existing ledgers do not mechanically cover a raw
   value reachable across an access-region boundary.
3. Decide the raw carrier's standard-trait policy before mechanical migration.
   `core::Value` currently exposes unqualified `Clone`, `PartialEq`, `Eq`, and
   custom `Debug`; these cannot be called access-qualified merely because most
   current callers happen to hold a mutator. Choose and document one coherent
   repair, such as removing those traits in favor of explicit access methods,
   or introducing a lifetime-branded regional view on which the operations
   live. Do not silently allowlist ordinary representation operations as
   collector primitives.
4. Extend the inventory's final mode so only regional-access APIs, genuinely
   non-escaping scoped exposure, durable boundaries, and the exact
   collector-mandated allowlist are accepted. Because unbranded `core::Value`
   is currently cloneable, a callback receiving one is not non-escaping merely
   because its reference is higher-ranked.

Exit: every violation is assigned, the standard-trait decision is explicit,
and the eventual zero-violation check can distinguish a real repair from a
renamed or newly allowlisted escape.

Completion record (2026-09-12): the syntax-backed raw-value inventory assigns
all 486 violations to exactly one D.2b-D.2g owner and one replacement shape.
The executable partition is 49 core carriers, 201 evaluator operations, 14
orchestration operations, 134 front-end operations, 48 reflection operations,
and 40 public/compiler/diagnostic operations. Within D.2b it distinguishes 40
core structural operations, six managed-cell operations, and three
runtime-root projections; within D.2g it distinguishes eleven durable public
boundaries from 29 compiler/diagnostic regional transformations. The existing
count and normalized signature fingerprint latch movement or replacement of
an occurrence. Durable-owner, containment, active-owner, recursive-identity,
and persistent-edge ledgers remain independently source-latched and are
reconciled at D.2h rather than collapsed into an imprecise combined count.

Standard-trait decision, 2026-09-12: remove unqualified `Clone`, `PartialEq`,
`Eq`, and recursive `Debug` from raw `core::Value`. Glam semantic equality,
reflection representation comparison, and diagnostic formatting become
separate access-qualified operations. `Key`, scalar semantic data, stable IDs,
and state enums retain their total relations. `RuntimeValueRoot`, public
`api::Value`, `EvaluatedValue`, and `Root<T>` remain clonable because managed
values share a registered root cell and inline immediates are self-contained;
they do not gain ordinary equality.

Persistent `Gc<T>` likewise becomes move-only and loses `Copy`, `Clone`,
`PartialEq`, `Eq`, and `Debug`. Duplication and allocation identity become
mutator-qualified, while collector-private `ErasedGc` remains the exact
copyable worklist identity. The additive migration, parent-phase interlock,
cutover, and verification are owned by the nested
[`GarbageCollectorPersistentEdgeTraits_2026-09-12.md`](GarbageCollectorPersistentEdgeTraits_2026-09-12.md)
plan. D.2a is not complete until its occurrence assignment is also complete.

##### GCI11R-002D.2b — Core Carrier and Structural Operations

Status: additive carrier work complete on 2026-09-12. The exact compatibility
declarations retained for downstream D.2c-D.2g callers are removed by those
phases and the nested persistent-edge plan's P4 cutover.

Migrate the 49 core-value, managed-cell, persistent-container, net-shell, and
runtime-root violations, including the standard-trait surface selected in
D.2a. Raw list/dictionary/value traversal, cloning, comparison, formatting,
and recursive-cell projection must carry `RuntimeValueAccess`,
`EvaluationValueAccess`, or collector-phase authority as appropriate. Prefer
one shared regional access threaded through a structural operation; do not
open a mutator independently for each member or edge.

For declarations selected by downstream code, this phase establishes and
latches the access-qualified replacement without removing the compatibility
entry point prematurely. D.2c-D.2g migrate those callers and retire the entry
points; nested P4 then removes the traits. Managed-cell and runtime-root
operations with no such downstream declaration interlock close here.

Verification: focused immediate/managed value controls, persistent list/dict
walks, lazy/promise/net identity operations, ordinary and aggressive tests,
and an updated occurrence manifest with no unassigned core-family violation.

P0-P2 of the nested persistent-edge trait plan completed on 2026-09-12. Its
closure inventory leaves only the exact P4 declarations and the fifty-five
core-carrier occurrences assigned here. D.2b-D.2g now remove the transitive
compatibility dependencies recorded by its P3 interlock; its P4 trait cutover
may occur only after those dependencies reach zero. Include the nested
checkpoint number in commits which perform its work rather than treating the
link as an unrecorded side transition.

###### GCI11R-002D.2b.0 — Carrier Topology and Cutover Latches

Status: complete on 2026-09-12.

Use D.2a's exact 40/6/3 partition and the persistent-edge plan's compiler
probes to record the carrier dependency topology before changing production
semantics. Separate declarations that *define* structural operations from
downstream calls owned by D.2c-D.2g. Additive access-qualified operations may
land here, but raw traits remain temporarily available until every dependent
family has migrated.

Exit: every D.2b declaration has an exact replacement family; the raw API and
persistent-edge inventories fail on drift; and no later phase can mistake
temporary trait availability for an accepted final boundary.

Completion record (2026-09-12): D.2a's executable manifest establishes the
40/6/3 declaration topology, while persistent-edge P2D assigns its 55
core-carrier trait/equality syntax occurrences to this parent migration. The
P2D compiler probes show that removing the edge traits is blocked only by the
`LazyValue`, `PromisedValue`, `CoreRuntimeNet`, `NetSpecialization`,
`NetValue`, and `FunctionCode` carrier closure. No collector worklist,
mutation descriptor, or direct managed-edge consumer remains in that closure.
The traits stay available solely as counted transition scaffolding until
D.2b-D.2g eliminate the carrier and call-site dependencies.

###### GCI11R-002D.2b.1 — Core Structural Access Surface

Status: complete on 2026-09-12 through D.2b.1a-D.2b.1d below.

Introduce the narrow access-qualified operations required to duplicate,
project, compare, and render `Value` and its list/dictionary/function/net
shells. Keep Glam semantic equality distinct from representation identity and
diagnostic rendering. Thread one borrowed `RuntimeValueAccess` through each
recursive walk. Do not add a blanket compatibility trait or open nested
mutators per element.

Verification: immediate and managed leaves, nested list/dictionary values,
functions and nets, and cycle-safe rendering/identity behavior where the
current operation promises it.

**D.2b.1a — Explicit shell duplication.** Add one
`RuntimeValueAccess::duplicate_value` operation which duplicates the outer raw
carrier under existing access. Scalar payloads copy or clone their ordinary
Rust data; persistent list/dictionary nodes, builtin argument arrays, sealed
metadata, and opaque handles share their existing immutable/RAII owner;
function, net, lazy, and promise shells duplicate their managed edge only
through the P1/P2 access-qualified gateway. Do not recursively copy shared
container contents merely to make the access visible.

Verification: every `Value` variant is exhaustively dispatched, immediate and
managed shells retain the old structural result, managed identity is
preserved, and duplication itself registers no collector root.

Status: complete on 2026-09-12. `RuntimeValueAccess::duplicate_value`
wildcard-freely dispatches every current `Value` variant. Direct lazy,
promise, net, and function-stage edges use the P1/P2 duplication gateway;
lists, dictionaries, builtin argument arrays, metadata, and opaque payload
handles share their immutable or RAII structural owner rather than recursively
copying contents. The focused fixture verifies immediate and managed shells,
exact managed identity, shared structural payloads, and an unchanged collector
root count. The raw API inventory gained exactly one regional operation while
all 486 violations and the persistent-edge manifest remained unchanged.

**D.2b.1b — Non-demanding inspection and key conversion.** Move outer-kind
classification plus raw `Value`/`Key` conversion beneath explicit access.
Keep conversion failure distinct from evaluation failure and do not demand a
lazy member while discovering that a structure is not keyable.

Status: complete on 2026-09-12. `RuntimeValueAccess` now owns outer diagnostic
kind inspection, recursive value-to-key conversion, and key reification.
Logical list traversal exposes borrowed byte segments as well as strict value
segments and deferred thunks; encountering a thunk makes key conversion return
`None` without invoking a forcing path.
Strict mixed byte/value containers round-trip through `Key`, while empty
dictionary fields retain their established elision rule. The old unqualified
methods remain counted compatibility shims until D.2c-D.2g migrate their
callers. The raw inventory gained exactly three regional operations, retained
all 486 violations, and the persistent-edge inventory did not change.

**D.2b.1c — Representation comparison.** Provide explicit access-qualified
representation comparison for reflection and bootstrap protocol use. Extend
persistent containers with borrowed comparator callbacks rather than relying
on `Value: PartialEq`. This is not Glam equality: managed identities compare
by exact allocation and unsupported semantic comparisons remain evaluator
policy.

Status: complete on 2026-09-12. `RuntimeValueAccess::same_representation`
exhaustively separates the retained-representation relation from evaluator
equality. Scalars and strict containers compare structurally; lazy, promise,
net, and function-stage edges compare by exact managed allocation; sealed and
opaque values preserve their hidden identity relations. Lists lend borrowed
logical items to the comparison callback, ignore byte/value leaf segmentation,
and represent a deferred tail by its exact thunk identity without forcing it.
The public reflection inspector and evaluated-value observer now delegate to
this regional operation. The raw inventory gained exactly one regional
operation, retained all 486 violations, and the persistent-edge inventory did
not change.

**D.2b.1d — Diagnostic rendering.** Replace recursive `Value: Debug` use with
an access-borrowing diagnostic view or formatter. Persistent containers must
delegate element rendering through that same access, and formatting must not
demand lazies, promises, or nets. Opaque and sealed payloads remain hidden.

Status: complete on 2026-09-12. `RuntimeValueAccess::diagnostic_debug` returns
a regional `Debug` view whose exhaustive value dispatch never falls back to
raw `Value: Debug`. Dedicated list, dictionary, and builtin-argument adapters
borrow nested values through the same access region. Byte segments remain
compact, while lazy values, promises, list thunks, nets, sealed metadata, and
opaque host values render only hidden outer labels and are never demanded.
The focused fixture covers nested containers and every hidden category under
ordinary and aggressive collection. The raw inventory gained exactly one
regional operation and retained all 486 violations. The persistent-edge
inventory now records the five exact adapter `Debug` implementations as
access-qualified observations rather than standard-trait defects; its 73
pre-existing cutover defects remain unchanged. The durable-owner source scan
assigns the two adapters which directly borrow `Value` storage to one exact
bounded-local diagnostic-view owner; they do not inherit the broad durable
recursive-payload classification.

###### GCI11R-002D.2b.2 — Managed Cells and Runtime-Root Projection

Status: complete on 2026-09-12 through D.2b.2a-D.2b.2e below, including the
cross-phase accounting inventories and immediate nested-construction repair.

Move the six managed-cell operations and three runtime-root projections to the
new surface. Root/public clones continue to share registered root cells;
projection of a raw value remains bounded by matching access. A managed cell
duplicates its persistent edge only through the P1/P2 gateway.

Verification: same/wrong-runtime controls, root-registration counters, and
aggressive collection before and after each projection or managed-cell
operation.

**D.2b.2a — Managed value storage.** Remove the authority-free managed-node
constructor, value projection, and factory-opening prepared-root constructor.
Construction and projection occur only beneath the caller's existing
`RuntimeValueAccess`; update the prepared-root same/wrong-runtime and
root-registration fixtures without weakening their checks.

**D.2b.2b — Halt payload construction.** Require matching regional authority
when an emission or context value becomes part of `EvaluationHalt`. This is an
explicit transitional ownership handoff, not permission to carry an unrooted
halt across a poll, callback, or durable-storage boundary.

**D.2b.2c — Promise publication.** Split access-qualified promise assignment
from post-region scheduler notification. Assignment is installed while the
managed cell is borrowed; completion and producer wakes are delivered only
after the caller has closed that access region and released mutation
admission.

**D.2b.2d — Runtime-root construction and projection.** Remove factory-based
root construction, observer-based raw construction, and owner-local raw
reprojection. Existing bounded regions publish through
`RuntimeValueAccess::root_runtime_value`; callers which currently own only a
factory or weak observer open one explicit local region. Preserve shared root
cells for root/public clones, and reconcile raw-access, durable-owner, and
persistent-edge inventories after the exact 6/3 violation set reaches zero.

Completion record (2026-09-12): all nine assigned violations are zero. Halt
payload construction and promise publication account for three new regional
operations; six authority-free managed-node/runtime-root operations were
removed. Promise assignment returns a must-use detached publication so wakes
occur only after managed access closes, with test builds asserting that
boundary. Root construction now uses either the
caller's existing `RuntimeValueAccess` or the factory's higher-ranked scoped
constructor; clones keep sharing registered root cells. The raw inventory is
586 declarations / 78 regional functions / 474 violating functions / eight
regional aliases / three violating derived traits, for 477 violations total.
The root-publication gate now counts legacy test construction, scoped factory
construction, and already-admitted publication separately, while the managed
admission, durable-owner, and persistent-edge gates remain closed.

Focused ordinary and aggressive managed-node, recursive-cell, and public
promise-resolver tests pass. The aggressive scheduler promise-wakeup fixtures
still use the test-only unrooted `PromisedValue::new` compatibility helper;
their fixture migration remains deliberately assigned to GCI11R-002E.

###### GCI11R-002D.2b.2e — Root Traffic and Mutator Introduction Accounting

Status: complete on 2026-09-12. Its two ledgers remain live cross-phase gates
through the broad D.2c-D.2g call-tree migrations.

D.2b.2a-D.2b.2d established the right construction primitives, but their
current inventories answer only part of the migration question. The
raw-value inventory assigns authority-free *signatures* to broad phases. The
root-publication and managed-admission inventories count syntax by source
file. None presently distinguishes a necessary outer boundary from a caller
which already has regional authority, nor does it reliably expose a nested
mutator entry hidden inside an otherwise access-qualified function. For
example, lazy-cycle terminalization currently calls
`construct_runtime_value_root` from inside an existing
`with_runtime_value_access` closure. That site is counted but is not assigned
as a redundant hierarchical admission.

Partition this work into the following independently latched audits and one
small repair checkpoint.

**D.2b.2e.1 — Runtime-root construction disposition inventory.** Status:
complete on 2026-09-12. Upgrade the
existing registered-root publication inventory from per-file text counts to
an exact, syntax-backed occurrence ledger. Record the containing declaration,
source location or stable syntax fingerprint, construction surface, phase
owner, and intended terminal disposition for every:

- legacy `RuntimeValueRoot::new` construction;
- `construct_runtime_value_root` and `try_construct_runtime_value_root`
  factory-scoped construction;
- `RuntimeValueAccess::root_runtime_value` publication; and
- equivalent helper which registers a new runtime value root without spelling
  one of those calls directly.

Separate production and test-only occurrences instead of allowing a
`#[cfg(test)]` call co-located in a production file to look like a production
boundary. Classify each production occurrence as exactly one of:

1. **outer construction boundary:** no access is available, and the value is
   constructed or safely projected from a still-live durable owner inside the
   higher-ranked closure;
2. **regional publication:** the containing operation already has matching
   access and must use `access.root_runtime_value` without opening another
   region;
3. **rooted transport migration:** an orchestration API presently receives a
   raw value only to root it and should instead receive/reuse
   `RuntimeValueRoot` from its producer;
4. **canonical constructor implementation:** the one reviewed implementation
   which opens the higher-ranked region and delegates to access-qualified
   publication;
5. **temporary compatibility or test fixture:** assign it to D.2E with an
   exact removal/migration condition; or
6. **defect:** no durable owner proves the captured raw graph live until the
   newly opened region begins.

Root clones which share one registered root cell are not new root traffic and
must remain distinguishable from registrations. Add representative
root-registration counter tests around evaluator completion, promise
publication, compiler/cache installation, and reflection handoff. These are
traffic invariants, not microbenchmarks: an existing root should cross a
boundary unchanged, and a same-region publication should add only the root
required by its final durable owner.

Completion record: the syntax-backed ledger distinguishes 44 production
occurrences from 62 test-only occurrences, names the containing declaration
and exact within-declaration ordinal, and assigns every production occurrence
to a D.2 owner and terminal disposition. It identifies the nested
`poison_lazy_cycle` construction as the one immediate defect and separately
records the reflection launcher's raw effect projection/re-registration for
D.2f rooted-transport migration; the freshly constructed initial reflection
state at the same `Branch::new` call site remains a regional publication.
Focused root-registration counters now prove that public evaluator completion
adds only its result root, promise success/failure reuse admitted roots,
reopening an installed compiler cache adds no roots, and the current raw
reflection-launcher handoff adds exactly one replacement root. The last
expectation is a latched migration baseline, not accepted terminal behavior:
D.2f must reduce it to no additional registration.

**D.2b.2e.2 — Mutator-introduction disposition inventory.** Status: complete
on 2026-09-12. Upgrade the
managed-access inventory to record every production and test-only mutator
introduction separately, including `with_runtime_value_access`,
`with_managed_values`, and the core domain's private direct
`Heap::with_mutator` gateways. Record the containing declaration, whether its
signature/receiver already carries `RuntimeValueAccess` or
`EvaluationValueAccess`, whether it is lexically nested inside another access
closure, and its D.2c-D.2g or D.2E owner.

Classify each introduction as an outer callback-free admission, the canonical
factory gateway, a deliberate recursive exception, a pending regional reuse,
a pending rooted-transport redesign, or a test-only compatibility site. The
source gate must reject an unassigned introduction and, absent an exact
reviewed exception, reject:

- a factory access opening lexically nested inside an existing access closure;
- a direct access opening in a function which already accepts a matching
  access carrier; and
- a helper on an access-carrying receiver which discards that authority and
  reopens the same heap.

This is a conservative source/call-site ledger, not whole-program call-graph
proof. Indirect hierarchy discovered while migrating a caller becomes a new
explicit occurrence rather than an excuse to weaken the gate. Existing rules
that no mutator crosses a callback, wait, scheduler handoff, or machine poll
remain authoritative; some outer admissions are therefore required and must
not be optimized away merely to reduce the count.

Completion record: the syntax-backed ledger records 38 production and 143
test-only introductions across the two factory gateways and the private
collector entry. It distinguishes the receiver and pre-call argument
evaluation from the body of the higher-ranked callback: only another gateway
inside that closure is lexically nested. Direct input authority is detected
through references, tuples, and generic wrappers, while
`impl FnOnce(RuntimeValueAccess)` correctly describes newly supplied authority
rather than authority already held by the caller. A focused synthetic fixture
latches both distinctions.

The reviewed production disposition is six canonical gateways (including the
two direct collector entries), eleven rooted-transport migrations, and
twenty-one justified outer admissions. There are no production lexical nests,
no production API which accepts matching access and then reopens the value
domain, and no recursive exception. The two detected lexical nests are
deliberate test fixtures and remain in the D.2E ledger. These classifications
are migration assignments, not a claim that all eleven raw projection seams
are already corrected.

**D.2b.2e.3 — Immediate nesting repair and phase interlock.** Status: complete
on 2026-09-12. First latch the
current redundant lazy-cycle nested construction, prove the access-depth/root
registration mismatch, then replace it with publication through the access
already held. Repair any other equally direct lexical nesting found by the
inventory only when its ownership is unambiguous; assign architectural API
changes to their owning D.2c-D.2g checkpoint instead.

Every later D.2 phase updates both ledgers as part of its exit. A migrated
entry must finish as a justified outer admission, access-qualified
publication, rooted transport, or removed fixture—not merely disappear from a
file-level count. D.2h closure requires no pending/defect disposition, no
unreviewed hierarchical introduction, and root-registration counter evidence
for each retained high-traffic boundary.

Completion record: a failing-first syntax-backed check isolated
`poison_lazy_cycle` as the only factory root construction nested inside an
active runtime-access callback. The exceptional successful terminal now calls
`access.root_runtime_value` directly, so it registers its required result root
without opening a second mutator region. The exact root ledger now classifies
that occurrence as a regional publication and reports zero immediate defects.
The mutator-introduction ledger independently reports no lexically nested
production gateway and no production API which directly receives matching
access before reopening the domain. Focused strict one- and two-node lazy-cycle
tests preserve terminal semantics; the structural latch covers the otherwise
unreachable successful-race fallback.

###### GCI11R-002D.2b.3 — Persistent Containers and Net Shells

Status: complete on 2026-09-12.

Replace implicit recursive `Clone`, equality, and `Debug` dependencies in the
raw list/dictionary and function/net carrier graph with the D.2b.1 operations.
This checkpoint changes operations, not the persistent-container
representation; compact/managed spines remain separately planned.

Verification: persistent sharing remains intact, duplication adds no roots,
and list/dictionary/net fixtures pass in ordinary and aggressive modes.

This work is partitioned because the generic persistent-list bounds, the
operations which actually duplicate list members, and the managed net-shell
traits have different failure surfaces. The final `Value`/`Gc<T>` trait
removal remains the nested plan's coordinated P4 cutover; these checkpoints
remove the structural reasons that cutover would otherwise fail.

**D.2b.3a — Shared-spine container traits.** Status: complete on 2026-09-12.
Make cloning the persistent list
shell depend only on cloning its shared spine, not on `V: Clone` or `T: Clone`.
Do the same for internal shared slices and finger-tree chunks whose clone is
only an `Arc`/`Bytes` operation. Keep element-producing methods bounded until
D.2b.3b supplies their explicit duplication operation. Latch the distinction
with a non-`Clone` element/thunk fixture and prove that the duplicate points to
the same spine. Dictionary shell cloning already has this property through
`rpds`; record and test it rather than wrapping or replacing that container.

Completion record: `List<V, T>`, its shared slice, and its finger-tree chunk
now implement structural cloning without `V: Clone` or `T: Clone`; the
remaining large inherent implementation keeps its element-producing bounds
for D.2b.3b. A compile-time generic fixture constructs and duplicates a list
whose element and thunk types implement no `Clone`, then proves both shells
name the same `Arc` spine. The access-qualified `Value` duplication fixture
now also proves list spine identity and RPDS dictionary root identity while
its existing collector counter proves that neither duplicate registers a
root. Both operation inventories remained unchanged.

**D.2b.3b — Element-producing persistent operations.** Status: complete on
2026-09-12. Separate list
structure-only operations from operations which return another `V` or `T`.
The latter accept borrowed duplication callbacks so core callers can thread
one `RuntimeValueAccess::duplicate_value` operation through traversal. Retain
ordinary `Clone` convenience only for genuinely external generic list users;
the `List<Value, ListThunk>` paths must use the explicit regional operation.
Verify lookup, split, front/back removal, balancing, shared leaves, and lazy
tails without adding registered roots.

Completion record: all structure-only list construction, concatenation,
splitting, balancing, traversal, and shared-spine operations now work without
`V: Clone` or `T: Clone`. `try_at_by`, `try_pop_front_by`, and
`try_pop_back_by` take a borrowed strict-value duplication operation and may
return a different output type; forcing remains a separate callback and
finishes before duplication of a value found behind that thunk. The former
implicit-clone entry points are test-only because this private generic module
has no production caller which can justify authority-free element copying.

Every production `List<Value, ListThunk>` extraction now supplies
`RuntimeValueAccess::duplicate_value` through a short evaluator access region.
The empty-list predicate supplies a no-op projection and therefore no longer
duplicates an element merely to discard it. A non-`Clone` value/thunk fixture
covers indexed lookup, both end removals, a forced tail, balancing, and
splitting; the D.2b.3a managed fixture continues to prove persistent
duplication adds no registered roots. Both operation inventories remain
closed at their prior counts.

**D.2b.3c — Function and net shells.** Status: complete on 2026-09-12.
Replace internal duplication,
representation comparison, and diagnostic formatting of `NetValue`,
`FunctionCode`, `FunctionValue`, `BuiltinCall`, and `ListThunk` with the
D.2b.1 access-qualified operations. The standard traits may remain only as
the exact, source-latched compatibility declarations needed by unmigrated
D.2c-D.2g callers; no new core operation may select them.

Completion record: `NetValue`, `FunctionValue`, `FunctionCode`, and
`BuiltinCall` now expose explicit access-qualified shell duplication as
needed, and the central `Value` duplicator delegates to those shell
operations. Net, function, and partial-builtin representation comparison
likewise delegates to shell-owned operations rather than selecting their
temporary equality traits. The existing regional diagnostic views already
hide these shells without invoking their `Debug` implementations.

`ListThunk` now owns access-qualified reification and exact managed-identity
comparison. Evaluator forcing uses the reification operation instead of
cloning `LazyValue` or `PromisedValue`. More importantly, compatibility list
tracing no longer constructs a temporary raw `Value` for a thunk at all: the
compatibility visitor has a direct managed-edge channel, and list thunks trace
their exact lazy/promise edge there. The persistent adapter's raw-value walk
therefore visits only values actually stored as values. Existing lazy/promise
tail reclamation fixtures latch the direct trace, while the focused shell test
covers function code, function stages, nets, partial builtins, and list thunks
without root registration.

The raw operation inventory now contains 589 declarations: 79 regional
operations, 25 collector primitives, 474 violating functions, eight regional
aliases, and three transitional derived traits. Its 477 violation set is
unchanged. The persistent-edge inventory retains the same 73 exact P4/parent
interlocks; no new implicit trait consumer was admitted.

**D.2b.3d — Container/shell closure.** Status: complete on 2026-09-12. Re-run
the raw-value and persistent-edge inventories plus compiler closure probes.
Update nested P3 with the exact
remaining downstream compatibility dependencies. This is a closure and
handoff checkpoint, not permission to remove `Value` or `Gc<T>` traits before
their coordinated P4 cutover.

Completion record: compiler probes removed the four shell traits that no
longer had a consumer: `FunctionCode: Clone` and `ListThunk: Clone + Eq +
PartialEq`. Negative probes keep `FunctionCode: Debug + Eq + PartialEq`
because `CoreOperator` still derives those traits, and keep `ListThunk: Debug`
because the legacy raw-`Value` formatter still formats list structure through
the generic list adapter. Those are downstream D.2c-D.2g interlocks, not
container requirements.

The persistent-edge inventory now contains 719 occurrences: 152 production
typed, 36 production erased, 517 test typed, and 14 test erased. Its 69 exact
cutover defects comprise five collector declarations, 13 managed-facade
declarations, and 51 parent/core carrier dependencies. A source-backed exact
manifest names every one of the 51 carrier dependencies by declaration and
operation, so removing a dependency requires updating the expected contract
and introducing a new implicit trait consumer fails locally. The raw-value
inventory remains at 589 declarations and 477 violations: D.2b's remaining
compatibility APIs are deliberately handed to D.2b.4 and the downstream
D.2c-D.2g migrations rather than hidden by this closure checkpoint.

###### GCI11R-002D.2b.4 — Parent Interlock Closure

Status: complete on 2026-09-12.

Update both occurrence inventories after each migrated declaration. This
checkpoint does not remove `Value` or `Gc<T>` traits while D.2c-D.2g still use
them. Instead it proves that all remaining persistent-edge compatibility
dependencies are owned by those downstream checkpoints and hands the final
zero-dependency condition to nested P4.

Exit: the managed-cell and runtime-root portions of D.2b's original 40/6/3
violation partition are zero; the core declarations retained solely for
downstream compatibility have an exact source-backed manifest and an
access-qualified replacement family; no structural declaration regains an
unreviewed authority-free operation; and nested P3 names only the carrier
traits retained until D.2c-D.2g and P4 close. Requiring every compatibility
declaration to disappear here would contradict this checkpoint's additive
staging and collapse the downstream phases into D.2b.

Completion record: the raw-value inventory contains no violation assigned to
the former six managed-cell operations or three runtime-root projections. At
D.2b closure it latched 40 core declarations by exact name. D.2c subsequently
removed six evaluator-only compatibility requirements; the current manifest
contains 34 occurrences: 31 functions plus the three `Value` derives for
`Clone`, `Eq`, and `PartialEq`. Changing that set now fails the D.2b-specific
manifest rather than merely changing a repository-wide count.

The persistent-edge owner vocabulary originally described 51 carrier traits as
the parent raw-value compatibility cutover, rather than incorrectly implying
that D.2b can remove them before their callers migrate. The current 59-entry
manifest includes eight later WHNF/net-driver carrier requirements reconciled
by the 2026-09-28 holistic review. D.2d-D.2g must update both inventories as
they remove callers; P4 removes the last traits once the manifest reaches zero.

##### GCI11R-002D.2c — Evaluator Operations and Builtins

Status: complete on 2026-09-27 through the focused resumable-WHNF plan.

Completion summary: the original evaluator partition is zero. Callback-free
operations receive caller-owned `EvaluationValueAccess`; every suspendable
path owns managed or rooted continuation state and reopens bounded access only
for a regional transition. W8 retired the direct evaluator gate and
whole-value wrappers, converted all whole-value fixtures to client demand, and
put the exact source and raw-value inventories into closure mode. The remaining
`EvalContext::evaluate_compatibility_whnf` declaration is owned by D.2d
orchestration, not this evaluator partition. Its production callers are
presently in D.2e front-end/macro code and D.2g diagnostics. D.2d therefore
narrows and inventories the bridge; D.2e and D.2g remove their callers before
D.2h makes it test-only or removes it.

Migrate the original 201 evaluator-operation and builtin violations as
call-tree families beneath `EvaluationValueAccess`. A callback-free evaluator
quantum opens one region and passes its authority through application,
operator, sequence, net, annotation, pattern, list, dictionary, object,
numeric, and effect helpers. Do not repair this family by opening hundreds of
nested mutators or by carrying a mutator through a wait, reflection gate, host
call, or scheduler handoff.

Verification: one compile-exhaustive evaluator/builtin access inventory,
focused builtin-family tests in both modes, and a source latch which rejects a
new authority-free raw helper in `src/eval/`. Resolve every D.2b.2e root or
mutator-introduction entry assigned to D.2c and preserve one outer admission
per callback-free evaluator quantum.

Partition decision, 2026-09-12: migrate by execution role and call tree, not
merely by source file. A file can contain all three relevant execution shapes:

1. A **regional operation** receives `EvaluationValueAccess` and may inspect,
   duplicate, or return raw values only within that active region. It cannot
   wait, invoke a host callback, or publish scheduler work.
2. A **suspendable step coordinator** receives `EvaluatorStepContext`, but that
   context is not treated as active value access. Durable inputs and outputs
   cross the boundary as roots, exact managed owners, or machine records; the
   coordinator opens short regional operations between waits and callbacks.
3. An **immediate-data helper** transports `Key`, `Number`, `Bytes`, stable
   IDs, or another representation proven not to contain managed edges. It
   should stop transporting `Value` rather than accept a ceremonial access
   parameter.

`EvaluatorStepContext` therefore must not be added to the raw-value scanner's
access-name allowlist. `EvaluationValueAccess` is the access-qualified carrier.
Likewise, merely calling `context.with_value_access` somewhere inside a broad
raw-value API does not qualify the API: its raw inputs must already be
protected, and no raw result may escape after the callback returns.

The original 201-operation baseline had two independent partitions. D.2c.1a
removed ten evaluator violations and deliberately handed five demandful
diagnostic compatibility operations to D.2g, leaving 191 D.2c operations and
472 repository-wide violations. Every later checkpoint must preserve the
updated exact manifests:

| Call-tree family | Operations |
| --- | ---: |
| Value demand, failures, and deferred sources | 19 |
| Application and sequence helpers | 12 |
| Core operators and runtime-net evaluation | 20 |
| Dispatch, scalar, comparison, and strategy builtins | 25 |
| Dictionary, list, and pattern builtins | 43 |
| Annotation, effect, and list-effect builtins | 42 |
| Object construction and linearization | 22 |
| Interaction-net builtins and construction callbacks | 8 |

By signature shape, 146 currently receive `EvaluatorStepContext`, seven
receive durable `EvalContext`, and 38 are context-free. These are census facts,
not accepted dispositions. In particular, the first group still lacks active
access and the last group may need either regional authority or a narrower
immediate-data signature.

This work is divided as follows. A checkpoint may update compile-required
callers outside its named source family, but it must not silently reassign
their inventory ownership or carry a mutator across that caller's
orchestration boundary.

###### GCI11R-002D.2c.0 — Evaluator Boundary Manifest

**D.2c.0a — Exact occurrence assignment.** Status: complete on 2026-09-12.
Extend the raw-value inventory so
each of the 201 operations has one exact D.2c subcheckpoint and one of the
three execution shapes above. Latch both the 29/12/20/25/43/42/22/8 family
partition and the 146/7/48 signature baseline by declaration rather than only
by aggregate count. A moved or renamed operation must identify its receiving
checkpoint.

Verification: deliberately misclassify one regional leaf and one suspendable
coordinator to prove the manifest rejects both errors, then restore the exact
assignment. Reconcile this manifest with `src/eval/access_inventory.rs`
rather than creating a competing list of evaluator contexts.

Completion record: the raw-value inventory now assigns every evaluator
violation to one of the eight D.2c family checkpoints by production source
owner. Per-family normalized declaration/signature fingerprints localize a
rename, move, or signature edit; the exact family counts remain
29/12/20/25/43/42/22/8. A second classification records the 146
`EvaluatorStepContext`, seven `EvalContext`, and 48 context-free signatures.
The conservative initial execution assignment treats all 153 context-bearing
operations as suspendable coordinators and all 48 context-free raw-value
operations as regional leaves. A family checkpoint may refine a coordinator
into regional leaves or eliminate an operation in favor of immediate data,
but must update this manifest explicitly. Synthetic boundary fixtures prove
that neither evaluator context is confused with active access and that a
context-free raw operation begins as regional work.

**D.2c.0b — Access and suspension gates.** Status: complete on 2026-09-12.
Ensure the raw-value scanner recognizes `EvaluationValueAccess` as regional
authority. Explicitly reject
using `EvaluatorStepContext`, `EvalContext`, or an internal
`with_value_access` call as a signature-level substitute. Add source latches
which reject a regional function that opens another mutator and a suspendable
function that accepts or returns an unrooted raw value.

Completion record: the scanner's existing canonical access set already named
both `RuntimeValueAccess` and `EvaluationValueAccess`; this checkpoint added
direct positive fixtures for both and negative fixtures for
`EvaluatorStepContext` and `EvalContext`. Recursive carrier discovery admits a
struct containing `EvaluationValueAccess` but rejects a structurally similar
step-context carrier. The D.2c manifest continues to expose all 153
context-bearing raw APIs as pending coordinator work rather than reclassifying
them. The independent mutator-introduction ledger already rejects any
production API which receives either active access carrier and then reopens
the runtime gateway, so no duplicate source scanner was added here.

Exit: every D.2c occurrence has a stable owner and execution shape; adding an
unclassified `src/eval` raw-value API fails locally; and subsequent
checkpoints can reduce their own exact partition without rebasing unrelated
families.

###### GCI11R-002D.2c.1 — Value Demand and Deferred Sources

Migrate the 29 operations in `src/eval/value.rs` first because every later
builtin family depends on their evaluation, failure, and collection helpers.
This is not one checkpoint: the file mixes pure construction with suspension
and durable machine state.

**D.2c.1a — Diagnostic and immediate shell helpers.** Move failure-diagnostic
construction, evaluation context frames, fallback diagnostic assembly,
deferred-kind tests, undefined tests, and split-result construction beneath
one borrowed access. Helpers which need only keys, atoms, numbers, or bytes
instead take those narrower types. Do not format through raw `Value: Debug`.

Status: complete on 2026-09-12.

Completion record: evaluator-local failure projection, fallback assembly,
context-frame construction, deferred/undefined shell inspection, and split
result construction now require one `RuntimeValueAccess`. `EvaluationFailure`
gained access-qualified emission/context borrows and context rebuilding, so
`EvaluationHalt::with_context` no longer selects the authority-free failure
clone. Production evaluator call sites either reuse an already-open region or
open a short inspection-only region; no region crosses evaluation, a wait, or
an effect callback.

The runtime-aware diagnostic normalization helpers can evaluate an emission,
so holding evaluator access across them would violate the suspension boundary.
They and the compatibility context constructors used by reflection/compiler
code moved to `diagnostic.rs`, where D.2g owns their eventual rooted/regional
cutover. This is a deliberate role reassignment rather than hiding a D.2c
violation: D.2c fell from 201 to 191 operations, the value-demand family from
29 to 19, and the D.2g compiler-diagnostic family rose from 29 to 34 while the
repository-wide violation count fell from 477 to 472.

Implementation ordering note, 2026-09-12: the remaining work must establish
the suspension boundary before migrating the projection call tree.  The
evaluator owns durable *outer* machines (`ClientDemandOperation`,
`LazyTaskMachine`, and `PromiseFollower`), but the recursive evaluator below
them is not itself a resumable machine. `eval_lazy_in` and `eval_promised_in`
therefore retain their Rust call stack while they reserve, pump, or wait on a
dependency. A regional operation cannot reuse that hybrid while retaining
value access, because its mutator would cross scheduler coordination.

An uncommitted D.2c.1d.1 prototype proved that returning only the exact rooted
lazy/promise is insufficient. A reflection step which constructed an
intermediate lazy retried from its preceding `MachineWork`, constructed a
fresh lazy, and suspended again without bound. The dependency was durable, but
the evaluator continuation which should consume its result was not. This is a
deterministic replay defect, not a scheduling race.

Consequently D.2c.1d begins with an explicit design checkpoint before
D.2c.1b-c. The selected continuation direction requires a resumable evaluator
or equivalent CPS/trampoline representation whose frames retain rooted state
between polls. Merely adding lazy/promise variants to `EvaluationHalt`, or
restarting the enclosing outer machine after their completion, is rejected.
The fallback is to preserve Rust-stack pumping and weaken the proposed
regional raw-value invariant; that does not implement the selected
continuation model and requires an explicit policy decision rather than an
accidental compatibility exception.

Planning update, 2026-09-12: the selected direction is now the standalone
[`Resumable WHNF Evaluation Plan`](ResumableWhnfEvaluation_2026-09-12.md).
It treats the missing state as one reusable, fine-grained WHNF submachine
hosted by the existing client-demand, lazy-producer, spark, net, and reflection
machines. `LazySource` remains the immutable semantic recipe; resumable
progress is separate, and the exact W0 inventory decides whether compact
source/operation phases suffice or repeated nesting justifies general
evaluation frames. D.2c.1b-D.2c.8 must perform their raw-value and control-flow
migrations once through that plan rather than landing recursive compatibility
wrappers for a second pass.

**D.2c.1b — List, key, number, and tagged-value projections.** Migrate list
front forcing, key conversion, tagged payload lookup, index/number extraction,
and semantic-undefined inspection. Preserve the D.2b.3 rule that the force
callback completes before the access-qualified element duplication callback;
no mutator spans lazy-list demand.

**D.2c.1c — Lazy, promise, and fixpoint regional work.** Move cached-state
inspection, source snapshotting, assignment projection, computed-fixpoint
construction, and successful/failed cache publication into short regional
operations. Reuse the machine's existing managed root or pending publication;
do not project and register a replacement root.

**D.2c.1d — Wait and reflection boundaries.** Refactor lazy-task completion,
promise following, deferred waits, and reflection-task evaluation so every
wait/reservation/callback occurs after regional access has ended. A resumed
step reopens access and reprojects from the durable owner. Latch both sides of
each ordering with barriers or probes; repeated runs are not evidence.

This is partitioned as follows:

- **D.2c.1d.0 — Recursive evaluator continuation decision.** Inventory the
  recursive call frames which may surround lazy, promise, reflection, net, or
  host suspension. Specify the smallest resumable evaluator state which can
  preserve those frames without retaining a mutator. Partition its migration
  by frame family before editing production behavior. Verification must
  include the reflection-annotation replay case which exposed unbounded fresh
  lazy construction. If this checkpoint instead preserves Rust-stack pumping,
  revise D.2c's regional invariant and root-traffic policy explicitly first.
  The detailed inventory, representation gate, implementation phases, and
  verification matrix now live in the linked Resumable WHNF Evaluation Plan;
  this parent checkpoint completed with W0 on 2026-09-12. The exact census
  selected a shared explicit work stack rather than proliferating
  source-specific phase enums; W1 begins the additive implementation.
- **D.2c.1d.1 — Durable lazy and promise suspension.** Extend the existing
  evaluation-halt/dependency vocabulary so a callback-free regional demand can
  return the exact rooted lazy or promise which prevented completion.  The
  regional operation performs no reservation, pumping, waiting, or wakeup.
  After its access region closes, the owning client-demand, lazy-task,
  promise-follower, spark, net, or reflection coordinator translates that
  disposition into the existing scheduled dependency.  Preserve the cheaper
  direct promise subscription for a resolver-owned unassigned promise; do not
  manufacture a follower solely to wait for assignment.
- **D.2c.1d.2 — Reflection suspension.** Separate reflection-task reservation,
  activation, polling, and acknowledgement from regional inspection and
  target projection.  A lazy task retains the durable reservation/handle or
  completed root between polls; no reflection callback or coordinator access
  occurs under `EvaluationValueAccess`.
- **D.2c.1d.3 — Regional demand cutover.** Introduce the callback-free regional
  value-demand entry and make the existing outer machine entry a continuation
  driver over its success or durable-suspension result.  Regional recursion
  passes one borrowed access through its call tree.  Scheduling a discovered
  dependency is an outer-machine action after that borrow has ended, not a
  recursive evaluator side effect.
- **D.2c.1d.4 — Boundary verification.** Add deterministic probes on both sides
  of lazy, promise, and reflection suspension.  They must prove the dependency
  is discovered while access is active and reservation/poll/wait happens only
  after access is closed.  Repetition remains stress evidence only.

The former provisional order was D.2c.1d.0, D.2c.1d.1-.3, D.2c.1b-c, and
D.2c.1d.4. The focused plan now supplies the authoritative order:

- its W0 phase completes D.2c.1d.0;
- W1-W3 implement the common computation state and lazy/promise/source
  control-flow ownership; W6A.0 closes the ten remaining non-compatibility
  D.2c.1 raw signatures;
- W4-W5 implement D.2c.1d.2 and the reflection/external boundaries;
- W6 performs D.2c.2-D.2c.8 control-flow conversion together with those
  checkpoints' raw-value migration; and
- W2E, W4D, W5D, and W7 collectively implement D.2c.1d.4 rather than deferring
  all ordering and stack verification to one late batch.

W8 closes the seven exactly inventoried value-demand compatibility declarations
and the temporary retryable-halt seams exposed during the conversion. This
mapping supersedes the former provisional sequence; the parent checkpoint
names remain the authoritative GC-remediation accounting.

Verification: focused `eval::value` and lazy/promise/fixpoint tests ordinarily
and with `aggressive-gc-verification`; root-registration counters across cache
hits, blocked polls, and resumptions; the poll-spanning owner inventory; and
the no-mutator-across-wait/callback source gates.

###### GCI11R-002D.2c.2 — Application and Sequences

**D.2c.2a — Application.** Migrate the eight application operations as one
call tree: dictionary application, function staging/instantiation, builtin
application, effect wrapping, and non-callable diagnostics. Recursive demand
returns to the step coordinator; argument aggregation and final graph
construction occur in a single regional leaf. Preserve partial application
and shared function-stage work.

**D.2c.2b — Sequences.** Migrate the four sequence operations together. Key
paths become immediate `Key` data; value-list extraction and append preserve
lazy tails and use explicit element duplication. A sequence helper must not
force a list while retaining access from the caller.

Verification: application/partial-application, dictionary application,
effect construction, append, key-path, and lazy-list fixtures in both modes;
no new function-stage or list-member roots.

###### GCI11R-002D.2c.3 — Core Operators and Runtime Nets

**D.2c.3a — Operator descriptors.** Migrate the nine context-free operator
constructors. Descriptor payloads containing values are built inside access
and installed immediately into the managed net; where a descriptor needs only
keys, arity, or stable node IDs, narrow its signature instead. Do not add a
durable unrooted staging descriptor.

**D.2c.3b — Operator execution.** Migrate the two remaining operator application and
effect-construction operations. One active net/value region covers an
individual callback-free reduction, then ends before the driver may park,
hand off a cursor claim, or report a dependency.

**D.2c.3c — Net claims, attachment, and extraction.** Migrate the seven remaining
`src/eval/net.rs` operations, including the raw claim projections. Claims stay
move-only; callable/operator payloads are inspected beneath matching access;
and cursor disturbance waits retain the existing special structural
coordination without turning into a general mutator-spanning wait. Preserve
batched disturbance publication and shared-net normalization.

The W6B.4b.2 callable-spill subcheckpoint may add the private runtime-only
`CallableCheckpoint`, but it must not add an ordinary duplication,
formatting, or equality trait dependency to the raw-value carrier closure.
Its focused NC0D checkpoint inventories
`NetSpecialization` bounds, `RuntimeNode` derives, whole-node clones, and
node equality/formatting assumptions before adding the variant. The
checkpoint requires `Send + 'static`, but not `Sync`: it moves between
mutex-protected net storage and one exact thread-bound claim. It is linear:
only `Bind >< CallableCheckpoint` reduces; fan, erase, and other principal
partners are stuck, and cursor copying waits for the source active pair rather
than duplicating its state. D.2c.3c owns the raw operation/count update, while
the nested persistent-edge plan's P3/P4 interlock owns the final proof that no
`Clone`, `Debug`, `PartialEq`, or `Eq` dependency was introduced.

The state remains part of the managed graph. Extend
`ManagedCoreNetCell::trace` through the runtime's logical-payload visitor to
walk every value-bearing checkpoint field and direct managed breadcrumb. A
boxed state is nested traced storage, not a root and not an excuse to omit its
edges. When a claim moves it out of the net, matching mutator admission must
remain active until the complete state is restored or published, preventing a
collector-visible gap without requiring `NetWhnfState: Trace + Sync`.

Verification: operator/function-call fixtures, net data extraction, cursor
handoff and contention barrier tests, and cursor WHNF tests in both modes.
Root and access ledgers must show no new net facade root and no nested
admission. The raw-value and persistent-edge inventories must reject a
trait-bearing checkpoint payload, a missing checkpoint edge, or a
checkpoint-copy path.

###### GCI11R-002D.2c.4 — Dispatch, Scalars, and Strategies

**D.2c.4a — Dispatcher, arity, assertions, and conditionals.** Migrate the
central builtin dispatcher and exact-arity extraction together with assertion
and conditional dispatch. The dispatcher threads an existing regional leaf
where the selected operation is callback-free; it returns to the step
coordinator before any selected operation may demand or suspend.

**D.2c.4b — Numeric and comparison operations.** Migrate the fourteen numeric
and comparison operations. Numeric/order helpers should accept `Number`,
`Key`, or other immediate semantic data after evaluated operands have been
projected. Recursive list/dictionary equality uses one access-qualified walk
and must remain distinct from representation identity.

**D.2c.4c — Strategy and provenance boundaries.** Migrate the six strategy and
provenance operations. `seq` and `spark` end access before scheduling or
waiting; provenance reflection follows the reflection boundary from D.2c.1d.
Remove the corresponding durable `EvalContext` shims once their admitted
callers use `EvaluatorStepContext` plus durable roots.

Verification: builtin arity/error, assertion, conditional, numeric,
comparison, `seq`, `spark`, and provenance tests in both modes, including
structured failure context and spark admission controls.

###### GCI11R-002D.2c.5 — Collection and Pattern Builtins

**D.2c.5a — Dictionaries.** Migrate the twelve dictionary operations: basic
selection/update/union, recursive merge, duplicate handling, and key-path
construction. Thread one access through each persistent transformation and
duplicate only values actually retained in the result.

**D.2c.5b — Lists.** Migrate the twelve list operations: indexing, end access,
split/slice, concatenation, mapping, length, and text lines. Use D.2b.3's
callback-based persistent-list extraction and prove forcing always happens
outside the access used to duplicate the resulting element.

**D.2c.5c — Patterns.** Migrate the nineteen pattern operations as one semantic
family. Literal/path comparison, dictionary take, list uncons/unsnoc, and
success/failure effect construction share the collection gateways from
D.2c.5a-b. Preserve mismatch as `.fail`, not an evaluation error, and preserve
optional-dictionary-key semantics.

Verification: focused dict/list/pattern tests and syntax-backed pattern
samples in both modes; persistent sharing and root-registration counters; and
explicit refutable remainder, computed path, and missing-key controls.

###### GCI11R-002D.2c.6 — Annotation and Effect Builtins

**D.2c.6a — Pure annotations.** Migrate annotation recognition, assertion and
value payload parsing, metadata input/output construction, and array/deque/
binary annotations that complete without reflection or strategy effects.
Sealed metadata inspection remains reflection-only.

**D.2c.6b — Reflection and strategy annotations.** Migrate deferred reflection,
`meta_refl`, `seq`, and `spark` branches separately from the pure annotation
leaf. Construct and publish the managed deferred result under access, end the
region, then reserve or schedule external work.

**D.2c.6c — General effects.** Migrate the seven effect dispatch/fixpoint/map
operations. Effect construction may be regional; interpreting an effect or
invoking an API returns to the step coordinator. Preserve ordinary freer
effect structure rather than treating it as a host callback.

**D.2c.6d — List effects.** Migrate the eleven list-effect operations remaining
after W3 established the lazy source owner, including the API leaf, dispatch,
`alt`, `cut`, fix, map, and deferred-tail handoffs. Semantic captures are
installed into their exact traced owner before access ends; no callback
closure may hide raw values.

Verification: annotation, metadata, effect, and list-effect suites in both
modes; reflection reservation ordering; metadata trace fixtures; and
alt/cut/backtracking tests proving abandoned branches publish no committed
history.

###### GCI11R-002D.2c.7 — Objects and Linearization

**D.2c.7a — Object construction and override.** Migrate object specification
construction, application, extension, instance creation, definition override,
and spec projection beneath explicit access. Persistent dictionaries retain
sharing and only installed values are duplicated.

**D.2c.7b — C3 and identity validation.** Migrate dependency discovery,
application order, C3 merge, object-name extraction, remembered-spec maps, and
referential-equality validation. Evaluation of a dependency ends access before
demand; the resumed comparison uses the exact managed identity operation.

Verification: object/abstract-object/extend samples, duplicate-name and C3
diagnostics, relative dependency overrides, and referential-spec identity
tests in both modes.

###### GCI11R-002D.2c.8 — Interaction-Net Builtins

**D.2c.8a — Regional graph construction.** Migrate the pure interaction-net
and arity builtin dispatch plus graph/port result construction. A completed
graph is installed beneath its managed outer node before access ends.

**D.2c.8b — Construction request boundary.** Migrate the callback-driven net
construction operations. Request callbacks receive rooted/public arguments,
perform no work under a mutator, and re-enter evaluation through the admitted
request context. Remove the remaining durable construction compatibility shim
when its caller is migrated.

Verification: direct-style net construction, function binding and partial
application, construction-port family, malformed request, and callback
re-entry tests in both modes.

###### GCI11R-002D.2c.9 — Evaluator Closure and Handoff

**D.2c.9a — Compatibility retirement.** Complete (2026-09-27). Remove the seven durable-context
signatures and the central direct-evaluator compatibility gate when no
production caller remains. Remove or narrow every context-free raw-value
helper. A test-only facade may remain only with an exact fixture disposition;
it cannot reopen production admission.

**D.2c.9b — Inventory reconciliation.** Complete (2026-09-27). Reduce D.2c's exact raw-value partition
to zero. Put `src/eval/access_inventory.rs`, the D.2b.2e root-traffic and
mutator-introduction ledgers, poll-spanning owner records, and relevant
durable/active-owner inventories into closure mode. Update D.2b's 40-entry
compatibility manifest and persistent-edge P3 whenever the last evaluator
consumer permits removing a core declaration; declarations still used by
D.2d-D.2g remain explicitly assigned there.

**D.2c.9c — Full verification.** Complete (2026-09-27). Run every family suite ordinarily and with
`aggressive-gc-verification`, then the complete required workspace checks.
Publish counts for removed raw APIs, retained downstream compatibility
declarations, mutator admissions, and root registrations. No race fix may rely
on repetition; ordering-sensitive claims require a barrier, probe, or model
test which forces both schedules.

Completion record: ordinary and aggressive W7 and D.2c closure suites pass,
the exact inventory suite passes, and the complete ordinary workspace passes.
The complete aggressive workspace command still fails across D.2d-D.2g
production owners and GCI11R-002E-H fixture/schedule/certification work, and it
does not settle after those failures. That is retained as the I11D.1/Gate G3
blocker rather than attributed to the now-empty D.2c partition. See the linked
W8 review for exact evidence and dispositions.

Exit: `src/eval` has no authority-free API which accepts or returns raw
`core::Value`; callback-free work uses a passed `EvaluationValueAccess`;
suspendable work uses durable owners and opens bounded access islands; no
mutator spans a wait, reflection gate, scheduler operation, or host callback;
and the inventories hand only D.2d-D.2g dependencies forward.

##### GCI11R-002D.2d — Evaluation Orchestration and Runtime Records

The executable inventory originally assigned exactly 11 violations to this
phase. Ten have now migrated to access-qualified or rooted orchestration; the
one deliberately retained compatibility bridge remains assigned here until
D.2e and D.2g retire its production callers. The migration used the following
low-risk checkpoints:

1. **D.2d.0 — exact manifest and bridge topology.** **Complete
   (2026-09-28, holistic review HR6).** Freeze the eleven
   declarations by name. Record every production caller of
   `evaluate_compatibility_whnf`: currently only D.2e macro/parser paths and
   D.2g diagnostics use it; the many evaluator/compiler/reflection occurrences
   are test-only. Recheck the root-registration and mutator-introduction
   ledgers before changing code.
2. **D.2d.1 — promise terminal publication.** **Complete (2026-09-28).**
   Migrate the two
   `PromiseProducerObligation::publish_assignment_*` operations and
   `promise_assignment_terminal` as one terminal-publication family. Preserve
   exact producer-ledger ownership and force collection on both sides of
   publication.
3. **D.2d.2 — reflection-task admission.** **Complete (2026-09-28).** Migrate
   `ReflectionTaskLauncher::build` and `EvalContext::reserve_reflection_task`.
   Their effects cross admission as roots or exact traced owners; no mutator
   spans task construction or host integration.
4. **D.2d.3 — bounded evaluation projections.** **Complete (2026-09-28).** Migrate
   `EvaluatorStepContext::{project_root,root_value}` and
   `EvalContext::{clone_root,compose_builtin,evaluate_builtin_whnf}`. Prefer
   the root already held by orchestration and one callback-free access island;
   do not project and re-register an equivalent value. `clone_root` and the
   obsolete lazy-construction helper were removed; construction and projection
   callbacks now consume their raw values inside the caller's one bounded
   access island, and evaluated builtins return the already-owned result root.
5. **D.2d.4 — compatibility-bridge handoff.** **Complete (2026-09-28).** Keep
   `evaluate_compatibility_whnf` as one exact, source-latched violation while
   D.2e and D.2g still have production callers. Those phases replace their
   calls with rooted input and access-scoped result consumption. D.2h then
   removes the production facade or narrows it to explicit test support before
   requiring zero production violations. D.2d must not duplicate that
   orchestration separately in each caller family. The active source latch now
   accepts exactly this declaration and rejects any second D.2d violation.

Audit task, client-demand, spark, wait, failure, event/output,
interaction-net, and yielded/blocked machine records while migrating these
families. Orchestration remains mutator-free while it pumps, waits, or invokes
integration: it transports roots or exact traced owners, then opens bounded
access only inside one callback-free poll or projection step.

Verification: exact owner-retirement and cross-poll tests, the existing
machine-state inventories, one explicitly counted compatibility bridge until
D.2e/D.2g retire its production callers, and aggressive tests which force
collection on both sides of every repaired handoff. Resolve every D.2b.2e
entry assigned to D.2d; orchestration which already owns a root must transport
it rather than project and register a replacement.

Completion record: the raw-value inventory now assigns one violation to D.2d,
the intentional facade above, versus eleven at entry. Its repository totals
are 581 raw operations, 308 access-qualified regional operations, and 233
remaining violations. The ordinary exact inventory, 172 reflection-machine
tests, and 48 diagnostic tests pass. Aggressive verification passes the
production reflection-launcher, type-erased effect-task, and diagnostic
partitions. Running the entire reflection-machine fixture module aggressively
also exposed deterministic test-only constructors which retain raw values
across their first verified access; those witnesses belong to
GCI11R-002E's fixture migration rather than reopening this production
orchestration phase. The first complete ordinary gate also exposed a regional
key conversion being polled again after terminal completion under forced
compiler-cache concurrency. A deterministic terminal-replay regression now
latches that mismatch, and the regional conversion caches and replays its
terminal key just as its managed wrapper already did. The complete ordinary
workspace gate passes after the repair.

##### GCI11R-002D.2e — Built-in Front End and Compiler Values

Migrate the currently inventoried 137 `g_syntax` violations by threading a shared
`RuntimeValueAccess` through semantic lowering, resolution, embedded-data
handling, compiler-value composition, macro result installation, and
diagnostic-value construction. Syntax ASTs remain ordinary Rust data; this
checkpoint must not blur syntax expressions into semantic/runtime values.
Closed and cached computations retain `RuntimeValueRoot` until a caller with
matching access installs or inspects the result.

Verification: compiler/macro/cache source inventories, executable `.g`
samples, ordinary and aggressive `g_syntax` partitions, and collection at
representative lowering, macro, and cache publication boundaries. Resolve the
D.2e root/admission ledger entries while retaining genuinely outer compiler
and macro boundaries.

Execute this phase by call tree rather than as one mechanical signature edit:

1. **D.2e.0 — exact manifest. Complete (2026-09-29).** The source-backed
   inventory assigns exactly 137 violations to D.2e: 30 compiler-value and
   diagnostic-facade declarations, 60 expression/pattern/effect resolution
   declarations, 34 module-definition/object/import declarations, and 13
   parser/macro orchestration declarations. Keep these four partitions exact
   while migrating them.
2. **D.2e.1 — compiler values and diagnostic facade. Complete (2026-09-29).** Give cached helper
   construction and `ResolvedExpr<Value>` composition one caller-owned access
   region. Cached and externally returned results remain roots; raw projections
   are consumed before the region closes. Compiler helper construction now
   builds and roots a closed input within one caller-owned access, releases
   that access before WHNF evaluation, and retains only runtime roots in the
   cache. Purely structural effect, module, annotation, and macro-environment
   applications may instead publish an unevaluated rooted result directly.
   Compiler-value and formatter composition is access-qualified, while
   diagnostic emissions cross into the still-pending compiler boundary as
   roots. This converts all 30 planned declarations, removes the obsolete
   `apply_closed` raw facade, and leaves 106 D.2e violations.
3. **D.2e.2 — resolution graph. Complete (2026-09-29).** Thread the same access through conditional,
   do, effect-step, expression, pattern, and scope lowering. The resolved IR is
   regional semantic data, not syntax and not a new durable value carrier.
   Conditional, do, effect-step, expression, pattern, and scope lowering now
   share the caller's access. Test-only convenience adapters remain visibly
   test-only; production resolved IR cannot cross the regional boundary.
4. **D.2e.3 — module lowering. Complete (2026-09-29).** Carry the regional authority through
   definitions, object declarations, imports, and the module fixpoint. Import
   callbacks remain outside access; their returned roots are projected only
   after re-entry. Definition and object resolution, resolved-net lowering,
   and publication of the next definitions root now occur beneath one access
   instead of projecting definitions before resolution and reopening access
   to publish the result. These two checkpoints convert 92 declarations and
   leave exactly fourteen parser/macro violations for D.2e.4.
5. **D.2e.4 — parser and macro orchestration. Complete (2026-09-29).** Keep parser structures ordinary
   Rust data. Root embedded semantic values and macro effects across isolated
   evaluation or host boundaries, and reopen one region only for deterministic
   parser/lowering work. `SyntaxExpr::Embedded`, lexical embedded-data tables,
   staged macro effects/results, and declaration materialization now retain
   runtime roots. Macro lookup demands one rooted step at a time; parser
   projection and output installation occur only inside bounded regions.
   Diagnostic context construction roots both the source emission and compiler
   update before normalization, because a user emission may demand arbitrary
   work and must not be normalized beneath managed access.
6. **D.2e.5 — closure. Complete (2026-09-29).** Require zero D.2e violations, reconcile the root and
   mutator-admission ledgers, then run focused and complete ordinary/aggressive
   front-end verification. The syntax-backed inventory now assigns zero
   violations to `g_syntax`; its exact baseline is 553 declarations, including
   437 regional operations and 76 remaining violations owned outside D.2e.
   Both compiler-access inventories pass, all 454 ordinary `g_syntax` tests
   pass, and 33 of 35 aggressive macro-expansion tests pass. The two remaining
   tests complete macro compilation, then expose the independently owned
   list-effect checkpoint admission defect recorded in D.2h.0 while evaluating
   the compiled result. Two stale reflection-gate fixtures discovered by the
   same run now construct and retain their actual roots rather than depending
   on an unrooted test facade.

##### GCI11R-002D.2f — Reflection Machine and Store

Migrate the 19 reflection violations in the current executable inventory.
Reflection-machine decoding and pure
semantic substeps may use bounded access, while transaction state, query
responses, blocked branches, task effects, and store journals retain roots or
exact traced owners across commits, retries, waits, and callbacks. Reflection
privilege permits observation unavailable to evaluation; it does not permit
unrooted GC pointers or authority-free raw-value manipulation.

Verification: reflection lifecycle/request/store inventories, retry and
rollback tests in both modes, and forced collection before and after query
publication and blocked-machine resumption. Resolve all D.2f construction and
admission entries, distinguishing pure regional reflection work from rooted
transaction/wait handoff.

Use the following checkpoints, ahead of D.2e where useful because these APIs
also serve macro-effect execution:

1. **D.2f.0 — exact manifest. Complete (2026-09-29).** The live inventory
   assigns 19 declarations to D.2f: twelve in the effect machine, two protocol
   constructors, three request helpers, and two store helpers.
2. **D.2f.1 — effect-task and branch admission. Complete (2026-09-29).** Replace raw effect/context
   handoffs with roots, or require the caller's existing access when the value
   is consumed synchronously. Keep raw-value convenience constructors test-only
   if production no longer needs them. Effect-task constructors now accept the
   root already owned by `api::Value`, isolated searches clone that root, and
   contextual task wrappers take a root directly. This removes five raw
   admission declarations without introducing an intermediate root or a
   project-and-reroot cycle; fourteen D.2f declarations remain in the regional,
   protocol/request, and store checkpoints below.
3. **D.2f.2 — regional machine helpers. Complete (2026-09-29).** Access-qualify effect API assembly,
   request construction, branch delivery, diagnostic context construction,
   alternative aggregation, and lazy-path construction. A helper returning a
   root must publish it before its access island closes. Request/API builders,
   branch delivery, unit delivery, dispatch contexts, and nested API edits now
   share explicit caller access. Specialized alternatives reuse their public
   roots, and heap/volume path reads project existing store roots only within
   the access that publishes the lazy path result. All machine-local D.2f
   violations are gone; the seven remaining entries are protocol/request/store
   boundaries.
4. **D.2f.3 — protocol and request helpers. Complete (2026-09-29).** Access-qualify immediate request,
   status, severity, and task-context values without granting reflection code a
   general raw-value escape hatch. Request envelopes, task-join frames, status
   query values, and severity matching now require the active access region.
   Status publication roots the finished encoding directly, while effect
   tokens and task-failure contexts reuse existing public/runtime roots. The
   obsolete authority-free context method and now-unused runtime-root
   projection helper were removed rather than retained as compatibility APIs.
5. **D.2f.4 — store boundaries. Complete (2026-09-29).** Consume raw edit/query values inside the
   transaction's bounded access; journals and query state retain public roots.
   Query decoding now roots the selected result before its evaluated-value
   access closes. Store edits and retirement updates project their existing
   public roots inside the single publication region; the authority-free
   wrapper was removed.
6. **D.2f.5 — closure. Complete (2026-09-29).** Require zero D.2f violations, reconcile reflection
   root/admission inventories, and run lifecycle, retry, rollback, blocked
   resumption, and aggressive publication tests. The executable inventory now
   assigns zero violations to D.2f, with fourteen reflection helpers converted
   to regional access and five rooted admission declarations removed. The
   reflection-machine, request, lifecycle, store, and raw-inventory partitions
   pass after preserving structured task-failure normalization at its
   callback-capable boundary.

##### GCI11R-002D.2g — Public API, Compiler, and Diagnostics

Migrate the currently inventoried public-API, compiler, source, and diagnostic
violations. Public boundaries transport `api::Value`, `EvaluatedValue`, or
another durable handle. Internal compiler and diagnostic transformations use
one explicit regional access and root their output before callbacks, logging,
imports, or returned diagnostics can outlive it. Keep reflection-only
observations visibly separate from reproducible evaluator/value helpers.

Verification: public API and compilation tests in both modes, import and
diagnostic callback boundaries, the D.1b no-reroot counter, and source latches
rejecting a raw public or host-callback signature. Resolve all D.2g entries in
the root-traffic and mutator-introduction ledgers; public handles should reuse
their existing root cells rather than cause hidden re-registration.

Use the following checkpoints. The live D.2g manifest is 44 declarations, not
the historical 45: ten public durable-boundary functions and 34
compiler/diagnostic regional functions.

1. **D.2g.0 — exact manifest. Complete (2026-09-29).** Add a source-backed declaration gate for the
   ten public and 34 compiler/diagnostic violations before changing their
   signatures. Freeze their replacement family separately so a public raw
   escape cannot disappear into an internal regional helper. The live gate
   freezes exactly ten public and 34 compiler/diagnostic declarations.
2. **D.2g.1 — public value boundary. Complete (2026-09-29).** Remove authority-free projection and
   wrapping helpers from `api::Value`, `EvaluatedValue`, `ValueKind`, and
   `Values`. Public operations either retain opaque rooted handles or perform
   inspection/construction under the existing `ScopedValues` access. Update
   callers without registering replacement roots for already-rooted values.
   Production code now projects through caller access or an existing public
   root; the former authority-free `wrap`/`clone_core` compatibility surface is
   test-only, and `EvaluatedValue::with_core` was removed.
3. **D.2g.2 — public diagnostic and assembly boundary. Complete (2026-09-29).** Make diagnostic
   envelopes accept and retain public values or runtime roots. Build the
   authoritative reflection environment within the assembler's existing
   scoped construction region. Change compiler diagnostic callbacks to carry
   a durable root rather than an unrooted core value. Diagnostic envelopes now
   retain public rooted values, readiness and assembly paths root before
   publication, the authoritative environment uses one caller region, and
   compiler callbacks receive a `RuntimeValueRoot` only after that region
   closes.
4. **D.2g.3 — source provenance and compilation trace. Complete (2026-09-29).** Give source/digest
   and trace-to-value constructors explicit caller access. Keep these
   constructors structural and non-demanding; they must neither open a nested
   region nor create a durable root on their own. Fourteen violations became
   sixteen explicit regional operations across source, trace, and the callers
   which project them; the remaining D.2g manifest contains 30 declarations.
5. **D.2g.4 — compiler context. Complete (2026-09-29).** Expose prior/final definitions and origin as
   existing runtime roots or project them only through caller access. Thread
   one front-end region through abstract-path, unit, import-failure, and
   diagnostic construction. Invoke diagnostic callbacks only after rooting
   their message and releasing regional access. Compiler-context projections
   now require the caller's access, while the diagnostic emitter accepts a
   `RuntimeValueRoot`; callbacks consequently begin only after the compiler
   region has closed. Ten violations became five regional operations and one
   raw callback alias disappeared. The remaining D.2g manifest contains 20
   declarations: six public boundaries and fourteen diagnostic helpers.
6. **D.2g.5 — diagnostic transformations. Complete (2026-09-29).** Split immediate structural
   construction/inspection into access-qualified helpers and demand-capable
   normalization into rooted operations. Remove the last production callers
   of `evaluate_compatibility_whnf`; never hold value access while evaluation,
   logging, or a host callback can run. Immediate message/context construction
   now requires caller access; transformation stages exchange runtime roots and
   explicitly close access before every evaluator or host-capable boundary.
   `evaluate_compatibility_whnf` is test-only and has no production caller.
7. **D.2g.6 — closure. Complete (2026-09-29).** Require zero D.2g violations and reconcile the raw,
   root-publication, mutator-introduction, durable-owner, containment, and call
   graph ledgers. Run focused public/compiler/diagnostic tests ordinarily and
   aggressively, then the routine repository gates. Record any downstream
   aggressive defect under its actual D.2h/E/F owner rather than weakening
   the D.2g closure. The executable manifest now has zero D.2g and zero D.2d
   violations; the remaining production compatibility surface is exactly the
   29 functions and three derived-trait occurrences assigned to D.2b. Public
   root/admission, durable-owner, persistent-edge, mutator-introduction, and
   resolved-call ledgers were reconciled to the new rooted boundary. Focused
   ordinary/aggressive diagnostic and compiler partitions pass, as do all
   eight raw-value and nineteen access inventories, the complete ordinary
   suite, clippy, formatting, and the interaction-net profiling gate.

   The ordinary gate also exposed that the claimed-exact-lazy fixture latched
   a pre-wait probe and required the first schedule-sensitive wake to be
   productive. It now latches the actual condition-variable wait; the blocking
   driver uses the exact-target wait helper, and the assertion requires every
   release to be classified while allowing intermediate relevant/unrelated
   disturbances before terminal publication. The parallel aggressive compiler
   discrepancy remains explicitly assigned to D.2h.0 rather than being hidden
   by its isolated pass.

##### GCI11R-002D.2h — Zero-Violation and Ownership Closure

Execution is partitioned so the final trait cutover is not hidden inside an
inventory relatch:

- **D.2h.0 — aggressive admission witnesses:** repair and latch the two
  production-shaped failures discovered by D.2g closure.
- **D.2h.1 — cutover readiness: complete 2026-09-29.** Run the raw-value, persistent-edge,
  root-publication, and mutator-introduction ledgers; reconcile every remaining
  entry with nested persistent-edge Phase P4. The executable pre-cutover
  baseline is 32 raw core-value violations and 77 persistent-edge defects:
  five collector traits, thirteen managed-facade traits/identity shims, and
  fifty-nine parent carrier dependencies. The root and mutator ledgers also
  pass their exact baselines after admitting the two new D.2h fixtures. These
  entries must be removed, not reclassified.
- **D.2h.2 — nested P4 cutover:** execute P4A-P4C from
  `GarbageCollectorPersistentEdgeTraits_2026-09-12.md`, including removal of
  the parent carrier traits, the managed-facade traits, and the five `Gc<T>`
  traits. Keep access-qualified diagnostics and collector-private `ErasedGc`
  distinct from the removed surface.
  - **D.2h.2a — production data-duplication seam: complete 2026-09-29.**
    Remove ambient `Clone` and representation equality from raw `Value`, its
    evaluation/failure/function shells, and `CoreOperator`. Route interaction-
    net payload duplication through an explicit mutation gateway and migrate
    compiler, evaluator, diagnostics, metadata, and reflection production
    callers to matching runtime access. The production library compiles at
    this boundary; test fixtures and the remaining runtime-source/facade/GC
    traits are deliberately owned by the following cutover checkpoints.
  - **D.2h.2b — fixture and runtime-source cutover:** migrate tests away from
    ambient raw-value traits, then remove the cursor/frontier/runtime-source
    compatibility traits through access-qualified duplication, comparison,
    and diagnostics.
    - **Fixture access vocabulary and core fixtures: complete 2026-09-30.**
      Test-only helpers now require an explicit `CoreValueFactory` and open
      bounded matching access for value, evaluated-value, failure, lazy,
      promise, net, and operator duplication or recursive representation
      comparison. They do not implement any removed standard trait. Core value
      fixtures use that vocabulary, access-qualified diagnostics/key/metadata
      observations, and `instantiate_with` for core payloads.
    - **Runtime-source production seam: complete 2026-09-29.** Generic runtime
      sources no longer carry ambient `Clone`, `Debug`, or equality bounds.
      Cursor dependency duplication and exact comparison are gateway
      operations; the core driver and its worklist duplicate source-bearing
      descriptors only through matching runtime access. Fixture migration and
      removal of the remaining core diagnostic facades remain open here.
    - **Production observation seam: complete 2026-09-30.** Raw values,
      evaluation failures, function/net/list shells, and core cursor facades no
      longer expose ambient `Debug` or representation equality. Detailed
      messages use `RuntimeValueAccess::diagnostic_debug`; public halt/root
      handles retain only opaque or display-derived Rust formatting. WHNF
      assertions use an edge-free result so panic formatting cannot restore a
      hidden raw-value observation dependency.
    - **Managed-payload fixture seam: complete 2026-09-30.** Compatibility
      payload, persistent-container, runtime-net, owner, containment, and
      value-node fixtures now duplicate and compare semantic values only
      through an explicitly named value factory/access region. The generic
      managed runtime-net fixture supplies its own mutator-qualified payload
      duplicator, and core-net reduction fixtures use the specialization's
      mutation gateway rather than falling back to the removed ambient traits.
      Test-expression lowering likewise borrows syntax and duplicates embedded
      semantic values through its lowering access instead of depending on
      `Clone` for either layer.
    - **Recursive managed-cell fixture seam: complete 2026-09-30.** Lazy,
      promise, and core-net publication/cycle fixtures now construct nets,
      duplicate assignments, compare terminal results, and report mutation
      transitions under their existing runtime access. Promise publication
      assertions no longer require `Debug` on rejected semantic payloads, and
      managed self-cycle construction uses explicit persistent-edge duplicates
      for every independently stored edge.
    - **Core-net fixture seam: complete 2026-09-30.** Direct fixture reductions
      now enter the core specialization gateway, payloads are duplicated under
      the fixture's value factory, and managed interface data is projected and
      compared only inside matching access. Schedule diagnostics retain stable
      scalar IDs without requiring recursive `Debug` on value-bearing step
      results.
    - **Compiler/runtime support fixtures: complete 2026-09-30.** Compiler
      context and runtime-failure-root fixtures compare semantic values through
      their owning factory; test diagnostic conversion delegates to the same
      access-qualified implementation as production; and list extraction
      duplicates strict segments and formats type failures only under the
      evaluator step's bounded value access.
    - **Evaluator/compiler-value fixture seam: complete 2026-09-30.** Lazy
      route, promise follower, compiler-cache, and macro-environment fixtures
      now duplicate and compare their semantic payloads through their owning
      evaluation context or compiler value factory. Cross-thread compiler
      helper tests explicitly duplicate the function edge before transfer.
    - **Diagnostic provenance fixture seam: complete 2026-09-30.** Test
      context-prepending now delegates to the production access-qualified
      transformation, while source-origin/import-chain fixtures copy list
      segments and compare nested diagnostic fields through the compiler value
      factory.
    - **Reflection fixture seam: complete 2026-09-30.** Reflection task/query
      fixtures now root borrowed core results through their poll context and
      compare decoded status/store payloads under the owning runtime. Public
      rooted values retain ordinary handle cloning; the change does not
      confuse that durable operation with raw core-value duplication.
    - **Remaining fixture equality migration — reviewed 2026-09-30.** Do not
      continue the remaining workspace cutover as an undifferentiated
      compiler-error rewrite. The current all-target compile reports 547
      distinct equality-error locations. Of those, 337 compare `Value`
      directly, 72 compare `Option<&Value>`, 57 compare `Vec<Value>`, and 13
      compare value slices; 516 are at or immediately within `assert_eq!`.
      These counts describe the present migration surface, not a permanent
      source baseline. Most errors are repeated consequences of one missing
      test assertion vocabulary.

      Use the value domain itself as the common assertion locus. Add a
      `#[cfg(test)]`, `#[track_caller]`
      `CoreValueFactory::assert_same_representation_for_test<L, R>` operation,
      where `L: SameRepresentationForTest<R>`, backed by the existing explicit
      test relation. It opens one bounded matching value-access region, creates
      no root, and reports the compared Rust type names plus runtime identity
      without restoring `Debug`, `PartialEq`, or `Eq`. Migrate the provisional
      free assertion function into this method rather than retaining two
      common factory entry points. A paired method on `RuntimeValueAccess`
      reuses an already-open region so assertions inside evaluator or managed
      mutation scopes do not recursively enter the heap. Both forms delegate
      to the same relation and remain visibly value-domain-qualified. Keep the
      Boolean relation available on both authorities for match guards,
      filters, and negative assertions. Do not introduce an exported assertion
      macro: a macro would improve expression spelling in panic text but would
      hide the value-domain authority which this cutover is intended to
      expose.

      Broaden only the test relation's structural adapters needed by existing
      fixtures: references, `Arc`, `Option`, `Result`, slices, vectors, and
      fixed arrays, including the common vector/slice/array cross-shapes. A
      recursive carrier which embeds values, such as
      `ResolvedExpr<Value>`, implements the relation in its owning module.
      Generic interaction-net fixtures likewise retain their own payload
      relation or scalar/topology assertion. Neither dependency is a reason
      for `core` to know front-end syntax or generic-net types. Public rooted
      facade tests continue to use their public evaluator/reflection observer;
      they must not reach through to this crate-private raw-value fixture API.

      Execute the remainder in reviewable checkpoints:

      1. **Assertion vocabulary implemented 2026-09-30; dynamic fixture pending
         compile closure.** Add the factory/scoped-access assertions,
         structural adapters, and focused positive, negative, nested-container,
         borrowed-value, and cross-shape tests. The recursive relation now
         receives the one scoped access instead of reopening a mutator for
         every leaf. Existing provisional free-function callers use the new
         authorities, including managed-cell assertions which reuse their
         surrounding access. The focused fixture latches maximum access depth
         one and zero root registrations. Production `cargo check --lib
         --all-features` passes and the all-target compiler reports no helper
         errors; executing the fixture remains dependent on completing the
         already-known workspace test migration below.
      2. Migrate the high-density `eval` and `evaluation` fixtures under the
         exact factory/context which owns each compared value. Keep missing
         duplication and failure-formatting repairs as separately visible
         work; the assertion helper must not become a general compatibility
         escape hatch.
         - **D.2h.2a — first evaluator fixture slice complete 2026-09-30.**
           The smaller WHNF/value-machine fixtures and the first bounded
           section of the central evaluator fixtures now assert semantic
           representation through their existing runtime value authority.
           Assertions already inside an evaluator access reuse
           `access.values()`; fixtures with a durable context use that
           context; singleton test-domain fixtures use `test_value_factory()`.
           The one enclosing `OperatorYield` comparison was destructured so
           only its semantic payload crosses the value assertion boundary.
           This slice reduces the all-target equality-error inventory from
           547 to 479 without hiding the distinct outstanding `Clone` and
           `Debug` migrations. The remainder of the central `eval` fixture
           and the `evaluation` lifecycle/coordinator fixtures stay in this
           checkpoint family.
         - **D.2h.2b — promise and deferred-work fixture slice complete
           2026-09-30.** Promise assignments, lazy caches, structured
           failures, list-effect resumptions, and cross-session wake fixtures
           now use the same scoped representation vocabulary. Test-only
           relations for bare `LazyValue` and `PromisedValue` facades compare
           their managed allocations under runtime access; recursive-cycle
           guards no longer depend on ambient facade equality. Assertions
           whose contract is exact cached-failure reuse remain explicit
           `Arc::ptr_eq` identity checks rather than being weakened to
           representation equality. This slice reduces the all-target
           equality-error inventory from 479 to 452.
         - **D.2h.2c — client-demand and lazy-fixpoint fixture slice complete
           2026-09-30.** Client-demand results, task-owned promise completion,
           computed fixpoints, lazy forwarding chains, and contended host-call
           fixtures now compare their semantic payloads under the owning
           value domain. `EvaluationWaitPoll::Complete` and
           `RuntimeValueRoot` are deliberately destructured and observed
           inside one matching runtime-access region rather than receiving
           broad test equality or projecting an unrooted value between access
           regions. Scalar wait tokens and exact shared-failure identities
           retain their existing ordinary Rust comparisons. This slice
           reduces the all-target equality-error inventory from 452 to 432.
         - **D.2h.2d — central list/container fixture slice complete
           2026-09-30.** Recursive dictionaries, mixed list segments, nested
           value vectors, failure-context lists, arithmetic results, mapped
           list items, and callable classification now use the same explicit
           value-domain relation. Byte buffers, list-shape statistics, and
           demand counters remain ordinary Rust comparisons. Nested
           `Vec<Vec<Value>>` exercises the structural assertion adapters
           without reintroducing semantic-value equality. This slice reduces
           the all-target equality-error inventory from 432 to 417.
         - **D.2h.2e — pattern and effect fixture slice complete
           2026-09-30.** Pattern pass/fail lists, dictionary decomposition,
           list front/back decomposition, text-line and split results,
           function application, partial builtins, and resumable effect
           dispatch now assert semantic payloads through their owning test
           factory or evaluator context. Counters, byte buffers, and textual
           failures remain ordinary Rust assertions. This slice reduces the
           central evaluator equality inventory from 132 to 86 and the
           all-target inventory from 417 to 371.
         - **D.2h.2f — dictionary and diagnostic-context fixture slice
           complete 2026-09-30.** Dictionary lookup, union, update and name
           resolution results now use runtime-scoped representation checks;
           missing paths use direct presence predicates. Annotation results
           and recursive diagnostic-context collections use the same test
           relation, while message strings remain ordinary comparisons. This
           slice reduces the central evaluator equality inventory from 86 to
           60 and the all-target inventory from 371 to 345.
         - **D.2h.2g — central evaluator fixture migration complete
           2026-09-30.** Associated-metadata projection, reflection-task
           results and gates, list annotations, and `seq`/`spark` strategies
           now compare semantic values through their live evaluator context.
           Sealed carriers required no new observation surface: the existing
           representation relation already preserves their opaque identity.
           Empty caches use presence predicates, while exact shared task and
           failure identities retain their narrower identity assertions.
           `src/eval/tests.rs` now contributes no equality errors; this slice
           reduces the all-target inventory from 345 to 285.
         - **D.2h.2h — evaluation coordinator and lifecycle fixture migration
           complete 2026-09-30.** Client-demand completion, deferred-work
           reclamation, task-promise publication, readiness snapshots, exit
           dispositions, and retained failure roots now compare through their
           owning runtime value authority. A test-only relation on the public
           `Values` service observes two public rooted values inside one
           bounded access region; internal `RuntimeValueRoot` assertions do
           the same without projecting an unrooted raw value between regions.
           `src/evaluation/tests.rs`, its coordinator fixtures, and the W7C
           fixture now contribute no equality errors. This slice reduces the
           all-target inventory from 285 to 237; the remaining `Clone` and
           diagnostic-formatting errors stay visible for their distinct
           migration checkpoints.
      3. **Owner-local front-end relation and fixture migration complete
         2026-09-30 (D.2h.2i).** `ResolvedExpr<V>` and its path components now
         implement the test-only representation relation in their owning
         module, recursively reusing one runtime access while leaving their
         ordinary generic syntax-only equality available. Conditional and
         `do` resolver fixtures, macro/reflection fixtures, source-evaluation
         fixtures, abstract-path assertions, and diagnostic-context searches
         use that relation or the owning compiler value factory. Public macro
         heap values use the assembler reflection observer rather than the
         crate-private raw-value fixture API. No `g_syntax` source now
         contributes an equality error; this checkpoint reduces the
         all-target inventory from 237 to 156. The remaining front-end
         duplication and formatting errors stay assigned to their distinct
         migration checkpoints.
      4. Migrate reflection, API, builtin-net, and generic interaction-net
         fixtures. Use public observers for public values and explicit local
         payload/topology relations for generic nets.
         - **D.2h.2j — evaluator-adjacent machine fixtures complete
           2026-09-30.** Access, builtin, resumable-WHNF, net-checkpoint, and
           small-stack W7B fixtures now compare values and value-bearing
           containers through their current `EvalContext`,
           `EvaluationValueAccess`, or core-net access. Core-net interface
           data is duplicated while the existing net/value access is open,
           avoiding a nested test mutator. The callable-checkpoint failure
           assertion preserves its stronger contract by comparing the exact
           shared permanent-failure allocation rather than replacing it with
           structural value equality. These five fixture families now
           contribute no equality errors and reduce the all-target inventory
           from 156 to 125. High-density value and builtin-net fixtures remain
           in this checkpoint family; unrelated duplication failures stay
           visible.
         - **D.2h.2k — high-density value and builtin-net fixtures complete
           2026-09-30.** Lazy checkpoint handoffs, list-effect state,
           host/reflection continuations, interaction-net builder state,
           captured reset/shift continuations, construction diagnostics, and
         restored builder outcomes now compare under their existing
         evaluator or runtime access. Rooted machine completions are
         observed through `RuntimeValueRoot`; recursive failure contexts use
         the same value-domain relation while scalar journal lengths and
         counters remain ordinary Rust assertions. These two fixture files
         now contribute no equality errors and reduce the all-target
         inventory from 125 to 74. Their separately inventoried raw-edge
         duplication and net-identity calls remain visible for the
         subsequent ownership checkpoints.
         - **D.2h.2l — reflection-machine fixture equality complete
           2026-09-30.** Request and reset-stack decoding, direct effect
           completions, reflection environments, cross-session task
           observation, structured failure contexts, and owned-promise
           terminalization now compare semantic payloads through the owning
           core or public runtime authority. Public effect results remain
           public rooted values and are compared through the `Values`
           observer rather than projected into the crate-private raw-value
           assertion surface. Exact task roots and propagated failure
           allocations retain pointer-identity assertions. The reflection
         machine fixture now contributes no equality errors and reduces the
         all-target inventory from 74 to 35; its distinct raw-value
         duplication and diagnostic-formatting failures remain visible for
         the subsequent ownership checkpoints.
         - **D.2h.2m — public API fixture equality complete 2026-09-30.**
           Promise assignment, diagnostic enrichment and context, compilation
           origin, runtime-volume, retained-context, and public-value fixtures
           now compare through either the public `Values` observer or the
           owning internal factory according to the API layer under test.
           Public handles are never unwrapped merely to recover ambient raw
           equality; recursive diagnostic payloads remain within their value
           domain. The API fixture family now contributes no equality errors
         and reduces the all-target inventory from 35 to 16. Its remaining
         raw-value duplication failures stay assigned to the separate
         ownership migration.
         - **D.2h.2n — generic interaction-net equality complete
           2026-09-30.** Builder failures are destructured to their scalar
           port contracts; active-pair scheduling states are asserted by
           variant; and cursor dependencies are checked through their local
           topology identity. Generic nets gain no semantic payload equality
           and core gains no dependency on the test specialization. The
         all-target equality-error inventory is now zero. The remaining
         generic-net duplication and mutation-gateway errors stay visible
         for the subsequent ownership migration.
         - **D.2h.2o — non-observing test result extraction implemented
           2026-09-30; dynamic fixture pending compile closure.** A narrow
           test-only `ResultTestExt` extracts expected failures without
           requiring the successful semantic payload to implement `Debug`.
           The affected evaluator, API, reflection, syntax, and coordinator
           fixtures use it for `expect_err`/`unwrap_err` control flow; actual
           failure diagnostics remain access-qualified and no production
         result API changes. This removes 246 formatting-bound compiler
         failures, reducing the all-target error inventory from 1,003 to
         757. Expected-success extraction, explicit managed-edge
         duplication, and genuine diagnostic formatting remain separate
         work below.
         - **D.2h.2p — generic runtime-net direct gateway closure complete
           2026-09-30.** The non-core test specialization now names the
           direct mutation gateway whenever it duplicates call payloads,
           cursor dependencies, or source-frontier state. Generic shared-net
           helpers state the exact `Data`/`Operator` duplication bounds they
           exercise; core evaluation remains on its managed gateway. This
         removes the remaining implicit generic runtime-source operations
         and reduces the all-target compiler inventory from 757 to 732
         without restoring ambient traits to runtime-net carriers.
         - **D.2h.2q — non-observing successful-result extraction complete
           2026-09-30; dynamic fixture pending compile closure.** The same
           test-only result vocabulary now extracts expected successes without
           requiring a semantic failure payload to implement `Debug`.
           Compiler-selected `expect`/`unwrap` call sites migrated only where
           the standard helper introduced that forbidden formatting bound;
           ordinary scalar and host errors retain standard Rust diagnostics.
           This removes 228 more formatting-bound failures and reduces the
           all-target compiler inventory from 732 to 504. The remaining 93
         formatting errors are actual carrier/debug or container-
         duplication seams rather than generic result control flow.
         - **D.2h.2r — API and reflection ownership slice complete
           2026-09-30.** The remaining API fixtures now duplicate lazy,
           promise, origin, and recursive diagnostic values only through the
           runtime-local value factory that owns them. Reflection-machine
           fixtures likewise duplicate promise arguments, failure contexts,
           emissions, and context frames under their observer or evaluation
           access; the removed ambient `with_context` helper is replaced by
           `with_context_in`. Diagnostic assertion text reports scalar context
           counts rather than demanding raw semantic `Debug`. These fixture
           families now compile under the negative trait contracts and reduce
           the all-target compiler inventory from 504 to 484. The remaining
         failures are concentrated in evaluator, net, and syntax fixture
         ownership rather than requiring a new public duplication API.
         - **D.2h.2s — syntax fixture ownership slice complete 2026-09-30.**
           Embedded semantic data, lowered module dictionaries, final-module
           promises, path traversal, lazy output fragments, and imported
           builtin members now cross fixture ownership boundaries only by
           duplicating through the relevant compiler or evaluation value
           domain. Syntax diagnostics name semantic kinds instead of requiring
           raw-value `Debug`, and memoized interaction nets compare exact
           managed identity under compiler value access. The `g_syntax`
           fixture family now compiles under the negative carrier traits and
           reduces the all-target compiler inventory from 484 to 457. No
           syntax or lowering semantics changed.
         - **D.2h.2t — evaluator-adjacent machine ownership slice complete
           2026-09-30.** Resumable access, builtin, list, object-fixpoint, and
           tagged-payload fixtures now duplicate promise edges, retained
           values, and deferred closure captures through their owning isolated
           evaluation context. A runtime-root construction duplicates its
           promise while the supplied access is active, and strict conversion
           failure assertions no longer require semantic-failure `Debug`.
           These five fixture families now compile under the negative carrier
           traits and reduce the all-target compiler inventory from 457 to
           428. Their state-machine protocols and suspension boundaries are
           unchanged.
         - **D.2h.2u — WHNF support and coordinator ownership slice complete
           2026-09-30.** Focused WHNF application/access/checkpoint fixtures,
           lazy-source evaluation, coordinator admission, and the W7C capture
           callback now preserve semantic edges through their existing value
           access. Code already inside evaluator or runtime access uses the
           admitted mutator directly; re-entrant test closures capture the
           owning factory and duplicate explicitly on each invocation.
           Runtime-root handoffs duplicate under the named domain instead of
           cloning a bare value. These fixtures reduce the all-target compiler
           inventory from 428 to 407 without changing scheduling, yield, or
           ownership semantics.
         - **D.2h.2v — generic cursor-runtime ownership slice complete
           2026-09-30.** Generic remote-cursor reduction helpers now state the
           exact data/operator duplication bounds required by the direct
           mutation gateway. Local cursor dependencies are reconstructed from
           their copyable `NodeId` payload when a fixture needs both an
           installed and expected value; `CursorDependency` remains
           non-`Clone`. The generic runtime-net fixture family now compiles and
           reduces the all-target inventory from 407 to 396 without widening
           the production specialization or carrier trait surface.
         - **D.2h.2w — W7B small-stack ownership slice complete 2026-09-30.**
           Deep lazy, promise, reflection, spark, and structured-failure
           fixtures now duplicate semantic edges through their isolated
           context. Structured context construction uses `with_context_in`,
           and the evaluator capture callback duplicates under its admitted
           step access. Cross-thread resumption fixtures retain the
           `OwnedEvalContext` returned by the second owner instead of
           discarding it and then borrowing the moved first-owner binding.
           This clears the W7B fixture family and reduces the all-target
           inventory from 396 to 386 without changing the forced scheduling
           or small-stack witnesses.
         - **D.2h.2x — NC5 callable-checkpoint ownership slice complete
           2026-09-30.** A test-only, value-qualified
           `CoreRuntimeNet::test_reduce_pair` gateway centralizes the managed
           payload transition already required by core-net access. NC5
           fixtures instantiate templates under active access, duplicate
           captured lazy/promise/net edges through their factory, and rebuild
           cursor assertions without ambient carrier `Debug`. This removes all
           27 NC5 compiler failures and reduces the all-target inventory from
           386 to 359. Callable-checkpoint scheduling and topology semantics
           are unchanged.
         - **D.2h.2y — W4 value/checkpoint ownership slice complete
           2026-09-30.** Application, fixpoint, static-access, semantic-thunk,
           object, list-effect, construction, and alternative fixtures now
           duplicate retained values and promise edges through their isolated
           context. Re-entrant semantic callbacks duplicate captures on every
           invocation; runtime-root handoffs and exact memoized-net identity
           use the owning access. This clears all 32 W4 fixture failures and
           reduces the all-target compiler inventory from 359 to 327 without
           changing checkpoint replay or route-loss behavior.
         - **D.2h.2z — pure net-builder fixture ownership slice complete
           2026-09-30.** Builder-state, alternative, cut, reset/shift,
           checkpoint, and fix fixtures now duplicate semantic values through
           active builder access or the owning context. Outcome arrays and
           recursive diagnostic frames use the runtime representation
           observer rather than ambient value equality, and lazy key/prompt
           callbacks duplicate their captures per invocation. Backedge
           inspection duplicates payloads while net access is admitted. This
           clears all 43 pure-builder fixture failures and reduces the
           all-target compiler inventory from 327 to 284 without changing the
           builder state machine.
         - **D.2h.2aa — evaluator net-driver fixture ownership slice complete
           2026-09-30.** Net-driver, cursor-dependency, callable-checkpoint,
           and nested-net fixtures now construct and restart retained driver
           work under matching value access, duplicate deliberately repeated
           semantic edges through their owning value domain, and route
           test-only reductions through the explicit runtime-net gateway.
           This clears all 65 `src/eval/net.rs` fixture failures and reduces
           the all-target compiler inventory from 284 to 219 without changing
           net scheduling, checkpoint, or reduction semantics.
         - **D.2h.2ab — client-demand ownership slice complete 2026-09-30.**
           Foreground client-demand, exact-subscription, cross-session,
           promise-assignment, lazy-cycle, and result-retention fixtures now
           duplicate repeated semantic edges through their owning evaluation
           context. Re-entrant lazy callbacks duplicate their captured values
           per invocation, and structured failure construction admits the
           value domain while attaching context. This clears the first 31
           `src/evaluation/tests.rs` fixture failures and reduces the
           all-target compiler inventory from 219 to 188 without changing
           demand, wake, abandonment, or retry semantics.
         - **D.2h.2ac — evaluation-machine publication ownership slice
           complete 2026-09-30.** Managed lazy-root construction, worker
           promise publication, terminal task results, redundant deferred
           registration, nested scheduled evaluation, and patient-wait
           fixtures now duplicate semantic edges only under their owning
           access or context. This clears six more
           `src/evaluation/tests.rs` fixture failures and reduces the
           all-target compiler inventory from 188 to 182 without changing
           publication, collection, or scheduling behavior.
         - **D.2h.2ad — lazy-route and task-promise ownership slice complete
           2026-09-30.** Abandoned/reclaimed lazy routes, lazy-owned WHNF
           checkpoints, shared client/spark/background progress, recursive
           task promises, exact promise followers, and terminal-publication
           probes now duplicate semantic edges through the context that owns
           them. Re-entrant lazy sources duplicate captured results on each
           invocation. This clears 16 more `src/evaluation/tests.rs` fixture
           failures and reduces the all-target compiler inventory from 182 to
           166 without changing route retention, cancellation, or atomic
           terminal-publication semantics.
         - **D.2h.2ae — settlement, output, and spark ownership slice complete
           2026-09-30.** Deadlock-settlement failures, forced-kill lazy
           reclamation, task-owned promise settlement, client blockers, and
           spark/lazy sharing fixtures now duplicate edges through their
           owning context. One-shot promise construction channels consume
           their `Option` with `take()` instead of manufacturing a second
           edge, and failure comparison uses the value-domain representation
           relation. This clears the final ten `src/evaluation/tests.rs`
           failures and reduces the all-target compiler inventory from 166 to
           156 without changing settlement or wake ordering.
         - **D.2h.2af — early evaluator object/net ownership slice complete
           2026-09-30.** Wrapper applications, object construction and
           extension, lazy source retirement, raw/computed nets, net arity,
           and function-net observation fixtures now duplicate semantic
           edges through their owning context. The reusable reflection-task
           launcher explicitly duplicates its terminal fixture per build
           instead of deriving `Clone` across `Value`. This clears 17
           `src/eval/tests.rs` failures and reduces the all-target compiler
           inventory from 156 to 139 without changing evaluator semantics.
         - **D.2h.2ag — core lazy/promise/effect ownership slice complete
           2026-09-30.** Partial function stages, promised and deferred WHNF,
           structured lazy failures, list-effect sequence/cut/fix recipes,
           resolver forwarding, promise/lazy cycles, and task fixpoints now
           duplicate semantic edges under their owning context or active
           evaluation access. Reusable thunk callbacks duplicate captured
           outputs per invocation. This clears 31 more `src/eval/tests.rs`
           failures and reduces the all-target compiler inventory from 139 to
           108 without changing retry, caching, or cycle semantics.
         - **D.2h.2ah — fixpoint, forwarding, and list-fixture ownership slice
           complete 2026-09-30.** Computed fixpoints, guarded recursion,
           forwarding chains, lazy WHNF checkpoints, contended producers,
           recursive-dictionary helpers, list-segment collection, and binary
           extraction contexts now duplicate values through their owning
           access or test value domain. Segment collectors perform explicit
           per-value duplication instead of `cloned`/`to_vec`. This clears 28
           more `src/eval/tests.rs` failures and reduces the all-target
           compiler inventory from 108 to 80 without changing evaluation or
           list traversal behavior.
         - **D.2h.2ai — builtin and compiler-pattern ownership slice complete
           2026-09-30.** List concat/chunks, equality and callable
           classification, sealed metadata observations, map, compiler
           literal/path/dictionary/list patterns, text lines, partial
           builtins, net list literals, and effect apply/call/map fixtures now
           duplicate semantic inputs through their owning context. A reusable
           dictionary-prefix thunk explicitly owns a distinct promised-leaf
           edge from the leaf retained for later assignment. This clears 35
           more `src/eval/tests.rs` failures and reduces the all-target
           compiler inventory from 80 to 45 without changing builtin or
           pattern semantics.
         - **D.2h.2aj — dictionary, index, and metadata ownership slice
           complete 2026-09-30.** Dictionary union/duplicate resolution,
           list-at and split-end, assert-unit and index context, pure metadata
           initialization/update, lazy carrier validation, and reflection
           metadata projection fixtures now duplicate semantic edges through
           their owning context or active access. Metadata inspection now
           explicitly admits matching value access. This clears 21 more
           `src/eval/tests.rs` failures and reduces the all-target compiler
           inventory from 45 to 24 without changing metadata sealing or
           reflection-task sharing.
         - **D.2h.2ak — final evaluator-fixture ownership slice complete
           2026-09-30.** Strategy, annotation, reflection-result, structured
           gate-failure, partial-builtin diagnostic, metadata-carrier, spark,
           and completed-update fixtures now make every retained semantic
           edge explicit through the matching value domain. Reusable thunk
           callbacks duplicate their result under captured runtime access,
           while one-shot inputs are moved rather than copied. Two
           reference-level `.clone()` calls which had silently copied
           `&Value` rather than `Value` were removed. This clears the final 24
           fixture failures: `cargo check --all-targets --all-features` now
           reaches zero errors without restoring any ambient semantic-value
           trait.
      5. Re-run the all-target compile, inventory every remaining equality
         error by intended relation, and add a source gate rejecting
         `assert_eq!`, `assert_ne!`, direct `==`/`!=`, `contains`, or derived
         equality whose operand transitively includes raw semantic values.
         Only then resume the distinct duplication, formatting, and ownership
         parts of workspace compile closure.
         - **D.2h.2b.5 — semantic-relation gate complete 2026-09-30.** The
           zero-error all-target compile proves every former equality use has
           selected an explicit relation. Compile-time negative contracts now
           pin `Value`, `EvaluatedValue`, `LazyValue`, `PromisedValue`, and
           `EvaluationFailure` as neither `PartialEq` nor `Eq`. This is the
           exact compiler-backed gate for `assert_eq!`, `assert_ne!`, direct
           equality operators, `contains`, and equality derived by a carrier:
           all require one of those forbidden leaf traits. The existing
           syntax-backed persistent-edge inventory separately rejects a
           handwritten or derived equality implementation on a managed
           carrier, without banning ordinary equality for keys and other
           genuinely comparable data.
  - **D.2h.2c — managed facade and collector cutover:** remove the thirteen
    managed-facade traits/identity shims and the five `Gc<T>` traits, add the
    negative compile/source gates, and make the full workspace compile.
    - **Production trait seam: complete 2026-09-30.** `Gc<T>` no longer
      implements `Copy`, `Clone`, `PartialEq`, `Eq`, or `Debug`, and no longer
      exposes unqualified `ptr_eq`. The three managed edge facades likewise
      expose none of those ambient traits; the ten facade trait dependencies
      and their three hidden `ptr_eq` calls are gone. Compile-time negative
      trait contracts and source latches guard both layers. The production
      library and the complete `glam-gc` library suite pass at this boundary;
      workspace fixture migration and final inventory closure remain below.
- **D.2h.3 — ledger closure:** require zero raw violations, zero persistent
  typed-edge defects, no pending production root/admission disposition, and no
  unreviewed fixture exception. Update exact counts and fingerprints only from
  the resulting source.
  - **Pre-closure core-boundary audit, 2026-09-30 — decisions settled before
    migration.** The completed all-target trait cutover leaves exactly 26 raw
    API violations, all in `src/core.rs`: nine `CoreValueFactory` scalar/root
    projections, three `EvaluatedValue` conversions, two failure operations,
    two host-call capture operations, two key conversions, two lazy-application
    borrows, one metadata constructor, three `Value` helpers, and two
    structural helper functions. Do not turn this into a blind signature
    rewrite:
    - `CoreValues` currently registers runtime roots for six canonical atoms
      and each factory projection opens a new mutator merely to copy an atom.
      Because the atoms contain no managed edge, the preferred simplification
      is to retain a root only for initial metadata, construct canonical atoms
      through caller-held `RuntimeValueAccess`, and then migrate the factory
      call sites. Confirm this representation decision before changing the
      roughly 150 unit/scalar call sites. **Decision:** remove those six roots,
      retain the initial-metadata root, and require caller-held access for the
      canonical-atom construction path.
    - `EvaluationFailure::Display` reaches into its structured emission to
      recover a compatibility string/kind without value access. A Rust
      formatter cannot accept a mutator. **Decision:** do not cache a second
      eager display summary in the failure. Preserve a rooted structured
      failure at the public boundary; let explicit diagnostic projection
      perform any evaluation and later rendering policy. Plain host errors may
      continue to display their already-existing Rust text, while an
      evaluation-backed `api::Error` uses an edge-free constant classification
      for `Display`/`std::error::Error`. Audit and migrate callers which flatten
      an evaluation failure merely for convenience. The broader transitional
      `Diagnostic` message/line projection cleanup is deferred in
      `docs/plans/README.md` unless it blocks GC.
    - failure and host-call capture traversal currently serves three distinct
      roles: collector tracing, mutator-qualified rooting/observation, and
      structural tests. **Decision:** do not encode “no active mutators” at
      the fixture or payload API boundary. Collector traversal receives only a
      collector-created visitor and delegates to the existing private
      `trace_managed_edges` operations; ordinary rooting and observation use
      `RuntimeValueAccess`; tests verify the applicable access contract or
      liveness/reclamation outcome. The current visitor happens to carry STW
      mutation exclusion. A future concurrent or moving collector may define
      different marking or relocation visitors without changing this
      separation.
    The equality/source gate is already closed; these decisions concern raw
    transport and observation, not restoration of any ambient trait.
  - **D.2h.3a — canonical root reduction complete 2026-09-30.** `CoreValues`
    now retains only the managed initial-metadata root. Unit, object-reflection
    guard, tuple, and severity atoms are constructed as edge-free immediates
    by `RuntimeValueAccess`; their transitional factory methods delegate to a
    bounded access region until D.2h.3b migrates callers. The focused
    collection fixture now pins one root and one marked slot instead of seven,
    and verifies every named access constructor against its canonical atom.
    This slice deliberately leaves the raw factory declarations visible to the
    ledger rather than disguising the pending call-site migration.
  - **D.2h.3b.1 — production canonical-value access migration complete
    2026-09-30.** Production factory projections for unit, object-reflection
    guard, tuple, severity atoms, arbitrary keys, and recursive key values are
    removed. API, compiler, diagnostic, evaluator, syntax-lowering, and
    reflection callers now reuse their existing `RuntimeValueAccess` or
    public scoped access. The production library compiles with no replacement
    root or nested admission. Temporary `#[cfg(test)]` factory projections
    keep the fixture corpus compilable while D.2h.3b.2 migrates it by
    subsystem; they are explicitly not part of the accepted final surface.
- **D.2h.4 — dynamic closure:** run focused ordinary/aggressive ownership
  checks followed by the routine workspace gates and the complete aggressive
  workspace suite.
- **D.2h.5 — record:** publish the dated accepted-surface and verification
  record, and close nested P3/P4/P5 only to the extent evidenced here.

0. Repair the aggressive admission witnesses exposed by closure verification:
   - **List-effect admission: complete 2026-09-29.**
     `source_macro_layout_dedents_resume_inside_delimiter_groups` and
     `source_macros_write_nested_and_same_anchor_layouts`: macro compilation is
     already complete when these fail. The exact defect was earlier than the
     provisional checkpoint-admission diagnosis: a source-owned
     `ListEffectComputation::{Sequence, FlatMapResults, FirstResult}` walked
     strict list values but omitted the list's direct managed thunk edges.
     Consequently a deferred list-effect child could be reclaimed while its
     owning lazy still retained the recipe, and a later checkpoint merely made
     that already-stale edge visible. `ListEffectComputation` now reports those
     thunk identities through its direct compatibility channel, and
     `LazySource` composes that channel into the managed lazy-cell trace. The
     independent fixture forces collection while the recipe is still the sole
     durable owner, then admits the checkpoint and collects again. It and both
     macro witnesses pass ordinarily and aggressively without placing a root
     inside the managed graph.
   - **Invalid-import fixture: complete 2026-09-29.**
     `compiler::tests::invalid_local_request_never_reaches_the_loader` passed
     in an isolated aggressive process but failed in the parallel aggressive
     compiler partition while rooting a pointer already pending finalization.
     The cause was shared test interference: `CompileContext::default()` used
     the process-wide compiler-test heap, while the test-only `import_module`
     wrapper returned a raw lazy after closing its construction access. An
     unrelated collector could therefore retire that fixture before
     `evaluate_compatibility_whnf` registered its replacement root. The
     repaired fixture uses a private value domain, publishes the import value
     as `RuntimeValueRoot` in the construction region, and demands it through
     `evaluate_root_whnf`. Its loop forces both relevant serial orders directly
     (demand before collection and collection before demand), so the former
     parallel-only `PendingFinalization` symptom is no longer accepted as a
     repetition-based race check. Both orders pass ordinarily and
     aggressively; no production handoff changed.
1. Run the syntax-backed API inventory in closure mode and require zero
   `Violation` occurrences. The accepted collector primitive set remains an
   exact declaration allowlist rather than a path-prefix escape hatch.
2. Reconcile the raw operation result with the durable owner, managed-edge,
   machine-state, callback-capture, and external-owner inventories. Every raw
   value stored outside an access region must be the unobserved payload of one
   exact traced/rooted owner.
3. Run every production-shaped exact test ordinarily and aggressively before
   migrating general fixtures in GCI11R-002E. A failure fixed only by changing
   a test constructor is not production closure.
4. Publish a short dated closure record listing the final accepted regional,
   scoped, durable, and collector-only surfaces and linking the source gates.
5. Put the D.2b.2e root-traffic and mutator-introduction inventories into
   closure mode: no pending/defect classification, no unreviewed nested access
   introduction, and no root registration whose final owner could have reused
   an existing root or the caller's active access.

Exit: already-rooted orchestration performs no project/re-root round trip;
every production raw-value API carries matching mutator/access authority or
exact collector-phase authority; every handoff is explicit; every durable raw
payload has one traced owner; the violation count is zero; and all
production-shaped exact tests pass aggressively. Gate G3 cannot pass without
this result.

### GCI11R-002E — Test Fixture Regional Migration

Two narrow evaluator fixtures were pulled forward by WHNFHR-001 because they
blocked the focused holistic WHNF aggressive gate. The forwarding-chain
fixture now constructs and publishes its graph in one access region, and the
abandoned-producer fixture retains its rooted promise/lazy owners. Their
ordinary/aggressive filters pass. This evidence reduces the eventual E.4
worklist but does not replace the source-backed inventory or close any other
002E item.

WHNFHR-003 subsequently pulled forward two more exact fixtures from the same
class. The hidden builder-reset fixture now uses rooted handoffs across each
construction/evaluation boundary and forces collection at all three former
gaps. The callable-checkpoint contention fixture now constructs and retains a
rooted core net before its worker interlock. Its overlapping-mutator schedule
runs in a private `NoAuto` domain, matching production policy; verification's
collection-before-every-entry policy would intentionally deadlock that barrier
shape and remains part of 002F's broader schedule audit. WHNFHR-004 also
updated one blocked-client fixture to advance foreground work through its
exact client driver rather than expecting the background-only runtime pump to
claim it. These focused repairs reduce E.4/F worklists but do not close them.
The first 2026-09-28 full parallel run still observed the repaired whole-state
builder fixture even though its exact and focused filters passed. A subsequent
ordinary full run reproduced the failure constructively enough to identify the
cause: the fixture forced collection through the process-wide test value
factory while unrelated parallel fixtures retained raw values in that shared
heap. The collecting fixture now uses a private value domain; its exact
ordinary/aggressive checks, the complete builder filter, and the complete
ordinary workspace pass. The two new rooted fixture helpers are also present
in the exact regional-constructor inventory. This closes that named witness
without weakening 002F's broader schedule audit.

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
7. Resolve every test-only or temporary-compatibility entry carried forward
   by the D.2b.2e root-traffic and mutator-introduction ledgers. Retained
   isolated helpers must have an exact fixture-only classification; general
   runtime fixtures must neither register replacement roots for already-rooted
   values nor reopen a mutator region inside matching active access.

Verification:

- `access_and_annotation_construction_do_not_demand_inputs` in both modes;
- API, evaluation, coordinator, and reflection-store test partitions in both
  modes; and
- a forced collection at each representative former constructor/root gap;
- root-registration counters proving migrated fixtures do not introduce
  replacement roots; and
- closure-mode source latches rejecting an unassigned test-only construction
  or nested mutator introduction.

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
5. Inventory fixtures which pass exactly or under a focused filter but fail in
   the full parallel aggressive suite. Reproduce each shared interference
   class with an injected admission/collector barrier before changing code;
   do not use repeated parallel runs as evidence. Determine whether the
   interfering state is a global verification hook, shared runtime resource,
   one-shot probe, or an unrooted publication exposed only by the forced
   ordering, then assign the repair to 002E or this phase accordingly. The
   whole-state builder witness discovered during the WHNF holistic review is
   closed by private-domain isolation and remains the reference example.

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
