# Aggressive GC Verification Remediation Plan — 2026-09-11

Status: GCI11R-002A-C and D.1a-D.2a complete; GCI11R-002D.2b-H planned. This plan expands
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

Status: planned as the checkpoints below. This is the parent implementation of
the nested persistent-edge plan's P3 interlock.

Migrate the 49 core-value, managed-cell, persistent-container, net-shell, and
runtime-root violations, including the standard-trait surface selected in
D.2a. Raw list/dictionary/value traversal, cloning, comparison, formatting,
and recursive-cell projection must carry `RuntimeValueAccess`,
`EvaluationValueAccess`, or collector-phase authority as appropriate. Prefer
one shared regional access threaded through a structural operation; do not
open a mutator independently for each member or edge.

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

Introduce the narrow access-qualified operations required to duplicate,
project, compare, and render `Value` and its list/dictionary/function/net
shells. Keep Glam semantic equality distinct from representation identity and
diagnostic rendering. Thread one borrowed `RuntimeValueAccess` through each
recursive walk. Do not add a blanket compatibility trait or open nested
mutators per element.

Verification: immediate and managed leaves, nested list/dictionary values,
functions and nets, and cycle-safe rendering/identity behavior where the
current operation promises it.

###### GCI11R-002D.2b.2 — Managed Cells and Runtime-Root Projection

Move the six managed-cell operations and three runtime-root projections to the
new surface. Root/public clones continue to share registered root cells;
projection of a raw value remains bounded by matching access. A managed cell
duplicates its persistent edge only through the P1/P2 gateway.

Verification: same/wrong-runtime controls, root-registration counters, and
aggressive collection before and after each projection or managed-cell
operation.

###### GCI11R-002D.2b.3 — Persistent Containers and Net Shells

Replace implicit recursive `Clone`, equality, and `Debug` dependencies in the
raw list/dictionary and function/net carrier graph with the D.2b.1 operations.
This checkpoint changes operations, not the persistent-container
representation; compact/managed spines remain separately planned.

Verification: persistent sharing remains intact, duplication adds no roots,
and list/dictionary/net fixtures pass in ordinary and aggressive modes.

###### GCI11R-002D.2b.4 — Parent Interlock Closure

Update both occurrence inventories after each migrated declaration. This
checkpoint does not remove `Value` or `Gc<T>` traits while D.2c-D.2g still use
them. Instead it proves that all remaining persistent-edge compatibility
dependencies are owned by those downstream checkpoints and hands the final
zero-dependency state to nested P4.

Exit: the D.2b 40/6/3 violation partition is zero, no structural declaration
regains an authority-free operation, and the nested P3 manifest names only
downstream D.2c-D.2g dependencies (or is ready for P4 if those have already
closed).

##### GCI11R-002D.2c — Evaluator Operations and Builtins

Migrate the 201 evaluator-operation and builtin violations as call-tree
families beneath `EvaluationValueAccess`. A callback-free evaluator quantum
opens one region and passes its authority through application, operator,
sequence, net, annotation, pattern, list, dictionary, object, numeric, and
effect helpers. Do not repair this family by opening hundreds of nested
mutators or by carrying a mutator through a wait, reflection gate, host call,
or scheduler handoff.

Verification: one compile-exhaustive evaluator/builtin access inventory,
focused builtin-family tests in both modes, and a source latch which rejects a
new authority-free raw helper in `src/eval/`.

##### GCI11R-002D.2d — Evaluation Orchestration and Runtime Records

Migrate the 14 remaining evaluation-orchestration violations and audit task,
client-demand, spark, wait, failure, event/output, interaction-net, and
yielded/blocked machine records. Orchestration remains mutator-free while it
pumps, waits, or invokes integration: it transports roots or exact traced
owners, then opens bounded access only inside one callback-free poll or
projection step. Remove `evaluate_compatibility_whnf` once its final caller has
moved to a rooted input plus access-scoped result consumption.

Verification: exact owner-retirement and cross-poll tests, the existing
machine-state inventories, no raw orchestration facade, and aggressive tests
which force collection on both sides of every repaired handoff.

##### GCI11R-002D.2e — Built-in Front End and Compiler Values

Migrate the 134 `g_syntax` violations by threading a shared
`RuntimeValueAccess` through semantic lowering, resolution, embedded-data
handling, compiler-value composition, macro result installation, and
diagnostic-value construction. Syntax ASTs remain ordinary Rust data; this
checkpoint must not blur syntax expressions into semantic/runtime values.
Closed and cached computations retain `RuntimeValueRoot` until a caller with
matching access installs or inspects the result.

Verification: compiler/macro/cache source inventories, executable `.g`
samples, ordinary and aggressive `g_syntax` partitions, and collection at
representative lowering, macro, and cache publication boundaries.

##### GCI11R-002D.2f — Reflection Machine and Store

Migrate the 48 reflection violations. Reflection-machine decoding and pure
semantic substeps may use bounded access, while transaction state, query
responses, blocked branches, task effects, and store journals retain roots or
exact traced owners across commits, retries, waits, and callbacks. Reflection
privilege permits observation unavailable to evaluation; it does not permit
unrooted GC pointers or authority-free raw-value manipulation.

Verification: reflection lifecycle/request/store inventories, retry and
rollback tests in both modes, and forced collection before and after query
publication and blocked-machine resumption.

##### GCI11R-002D.2g — Public API, Compiler, and Diagnostics

Migrate the remaining 40 public-API, compiler, source, and diagnostic
violations. Public boundaries transport `api::Value`, `EvaluatedValue`, or
another durable handle. Internal compiler and diagnostic transformations use
one explicit regional access and root their output before callbacks, logging,
imports, or returned diagnostics can outlive it. Keep reflection-only
observations visibly separate from reproducible evaluator/value helpers.

Verification: public API and compilation tests in both modes, import and
diagnostic callback boundaries, the D.1b no-reroot counter, and source latches
rejecting a raw public or host-callback signature.

##### GCI11R-002D.2h — Zero-Violation and Ownership Closure

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

Exit: already-rooted orchestration performs no project/re-root round trip;
every production raw-value API carries matching mutator/access authority or
exact collector-phase authority; every handoff is explicit; every durable raw
payload has one traced owner; the violation count is zero; and all
production-shaped exact tests pass aggressively. Gate G3 cannot pass without
this result.

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
