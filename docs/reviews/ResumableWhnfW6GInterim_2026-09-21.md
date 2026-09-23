# Resumable WHNF W6G Interim Implementation Review — 2026-09-21

Baseline: `e7cf6e0a`. Status: review closed by planned resolutions; W6G
implementation remains open.

Review procedure:
[`ResumableWhnfW6GInterimReviewPlan_2026-09-21.md`](../plans/ResumableWhnfW6GInterimReviewPlan_2026-09-21.md).

## Resolution status — closed 2026-09-21

The later [remaining-work plan review](ResumableWhnfW6GRemainingPlan_2026-09-21.md)
settled the disposition and order for these findings. Each finding is **closed
as a review item by an explicit corrective step and verification gate** in the
[parent plan](../plans/ResumableWhnfEvaluation_2026-09-12.md). The implementation
is still pending; closing this review is not evidence that the current code or
documentation already satisfies the target contract.

| Finding | Review resolution | Pending plan gate |
| --- | --- | --- |
| W6GIR-001 — session-affine deferred producer | **Closed by plan.** The route lifecycle and global-selector dependencies have explicit owners. | W6G.1f.3i removes the last state-bearing route payload; W6G.1f.2b.0-.4 changes and forces the coordinator lifecycle; W6G.1e.2/e.3 retires broad selection. |
| W6GIR-002 — name-only producer inventory | **Closed by plan.** The audit now requires constructor and payload-shape evidence, not just names. | W6G.1f.3i.0 inventories constructors and payloads; f.3i.3-.4 removes `Whnf` route state and checks the remaining ownership shape. |
| W6GIR-003 — missing coordinator-level retention races | **Closed by plan.** The route and mixed-observer schedules have explicit forced-order gates. | W6G.1f.2b.4 forces coordinator subscriber/claim races; W6G.1f.4b-d supplies integrated retention, mixed-observer, and collection evidence. |
| W6GIR-004 — present-tense architecture drift | **Closed by plan.** The two stale descriptions have a named documentation checkpoint. | W6G.1f.3i.4 updates `docs/architecture/evaluation.md` and `src/README.md` to describe the actual transitional route and completed W6G.3 state. |

W6GIR-001–004 require no further review-specific action. Their implementation
and verification remain obligations of those plan checkpoints, which should
not be marked complete on the strength of this review closure.

## Scope and outcome

This audits completed W6G.1 implementation and completed W6G.3 against the
current code and focused tests. It is deliberately distinct from the earlier
W6G.1 [baseline](ResumableWhnfW6G1Baseline_2026-09-18.md) and
[design review](ResumableWhnfW6G1Design_2026-09-19.md), which largely inspected
the *proposed* transition. It is also not W6G.5: W6G.1's route and selector
cutovers, W6G.4, integrated phase verification, and W7/W8 drift audit remain
later work. **Later disposition (2026-09-23):** W6G.2 moved to the independent
deferred
[Pure Effect Access Fusion plan](../plans/PureEffectAccessFusion_2026-09-23.md)
after Value Representation Refinement.

The implemented boundary is coherent as a staged system. Foreground client
records have a separate registry and workers no longer select them directly.
The managed lazy owns one source/checkpoint/result state, and migrated
families place demand-driven progress below traced checkpoint edges. An
activated reflection task instead owns its continuation and fulfills a
managed promise. W6G.3 has removed the old root-per-frame demand checkpoint:
one canonical managed cell now owns its WHNF focus and continuation state.

**This does not yet establish the W6G.1 target lifecycle.** The deferred
coordinator record still owns a machine and first-discoverer session; workers
retain a transitional global deferred fallback. These are named migration
states with existing owners, not newly discovered regressions or acceptable
final semantics. No new W6G.3 correctness defect was found in the inspected
construction, poll, trace, and collection paths.

## As-built boundary accounting

| Boundary | Current evidence | Disposition |
| --- | --- | --- |
| Foreground root selection | [`client_demands` is separate](../../src/evaluation/coordinator.rs); [`select_worker` and `select_runtime_pump`](../../src/evaluation/coordinator.rs) start from background records; `worker_and_runtime_pump_selectors_reject_foreground_client_demand` forces exclusion. | W6G.1b/e.1 implemented. The private foreground driver and public incremental-handle policy remain W6G.1d work. |
| Lazy semantic owner | [`ManagedLazyCell`](../../src/core/managed/recursive_cells.rs) retains `Source` or `Checkpoint` while unresolved and a one-write terminal result. Its trace prefers the result, otherwise visits the current producer state. Source-to-checkpoint installation and terminal publication use the collector edge-transition gateway. | W6G.1f.1 implemented. Temporary route roots do not themselves prove last-subscriber retirement. |
| Typed checkpoint families | [`ManagedLazyCheckpointKind`](../../src/eval/lazy_checkpoint.rs) is a concrete, exhaustively traced sum for WHNF, host call, net WHNF, access, object fixpoint, list effect, and builtin work. Representative route-loss, collection, and no-replay fixtures live in [`eval/value/tests/w4.rs`](../../src/eval/value/tests/w4.rs). | W6G.1f.3c-h migrations are implemented; the remaining state-bearing `LazyTaskWork::Whnf` path is W6G.1f.3i. |
| Autonomous reflection | [`LazyTaskMachine`](../../src/eval/value.rs) reserves a task-owned managed completion promise, installs an ordinary promised-WHNF checkpoint, and defers activation until evaluator access has closed. [`ReflectionTaskReservation`](../../src/evaluation/session.rs) owns the one-use activation/terminalization boundary. | Correctly distinct from a lazy-owned reflection continuation. Route loss, late root ownership, and all observer roles still need W6G.1f.4 integration. |
| Host and spark boundaries | A host call transfers `Before` to `Invoking` before calling Rust, retires the route's one-use permit, and stores its result as a traced `After` state; a resumed route cannot invoke it again. Builtin strategy progress records spark admission separately from post-access submission. | Focused no-replay proofs exist; W6G.1f.4 still needs integrated last-subscriber schedules. |
| Aggregate WHNF demand | [`WhnfComputation`](../../src/eval/whnf.rs) retains `Source`, a minimal `Seed`, or one `ManagedDemand` root. [`ManagedLazyCheckpointCell`](../../src/eval/whnf/managed_state.rs) holds canonical `WhnfState`; one bounded poll performs one edge transition and adds no per-frame roots. A returned terminal result has its own root until its caller releases it. | W6G.3 implemented. Seed/source distinction and brief seed-to-cell overlap are intentional, not extra steady-state roots. |

The family survey inspected the actual checkpoint carrier and representative
machine/trace dispatch, plus the source-backed durable-owner and active-owner
inventories. It is not a claim that a name-only enum latch proves each nested
field safe. In particular, arbitrary external host callback captures remain
conservatively owned outside the managed graph; the checkpoint traces its
explicit semantic captures and the host owner has its own reviewed boundary.

## Ownership, execution, and test assessment

- **Demand state and collector edges.** The managed WHNF cell's
  [`Trace`](../../src/eval/whnf/managed_state.rs) calls the same canonical
  `WhnfState` edge visitor used for mutation. The lazy's trace selects one
  authoritative source/checkpoint/result state. The reachable and unreachable
  self-checkpoint fixtures force collection, and W6G.3 tests compare one and
  32 retained frames while asserting one steady demand root. These support
  the completed representation contract; they do not exercise coordinator
  last-subscriber retirement.
- **Bounded polling.** `WhnfComputation::poll_in` promotes a seed under the
  caller's access, borrows the canonical state for a budgeted regional drive,
  and converts ready/dependency/yield/failure only at the poll boundary. The
  W6G.3 aggregate transition test counts one leaving/adding edge publication
  despite multiple internal edits. A cross-thread fixture resumes the same
  state after collection. The test-only exit matrix checks that earlier
  boundary fixtures still exist; their behavior is exercised by those actual
  fixtures, not proved by the string check alone.
- **One-shot effects.** Host-call tests count invocation on interruption and
  route recreation. Reflection tests cover terminal mapping, activation-root
  transfer, and ordinary promised-WHNF handoff. Spark tests cover queued,
  claimed, blocked, and abandonment lifetimes. Their separate success does
  not yet prove the mixed client/spark/reflection and final-subscriber matrix.
- **Inventory strength.** The typed checkpoint sum and `Trace` match are
  compile-exhaustive. The current [`lazy_producer_inventory`](../../src/eval/lazy_producer_inventory.rs)
  parses `LazyTaskWork` but compares only variant *names*, and checks three
  external boundaries by textual presence. It does not assert that a marker
  is fieldless, that `Whnf` is the only state-bearing route variant, or that
  the named operations are reachable along the intended source path. The
  newly partitioned W6G.1f.3i.0 and .4 correctly own this stronger audit.

Focused verification at this baseline passed: `cargo test -q --lib w6g3`
(13 tests), the same filter with `--features aggressive-gc-verification`
(13 tests), `w6g1` (2 tests), the exact lazy-work inventory, reflection
terminal mapper, foreground-selector exclusion, interrupted host-call
no-replay, public net-construction cycle reclamation, and aggressive-GC
unreachable lazy/checkpoint self-cycle tests. These are confirmation of those
fixtures, not a substitute for the forced orderings still assigned to
W6G.1f.2b and W6G.1f.4. No Rust code changed during this review.

## Findings and dispositions

### W6GIR-001 — High transitional risk: deferred records still carry first-observer ownership

[`reserve_deferred`](../../src/evaluation/coordinator/deferred.rs) stores the
first producer's `demand_session`, task, wait, and boxed machine. Its release
path terminalizes on that session's close, while the worker selector still
accepts globally ready deferred work. This is exactly the previously reported
route/session-affinity hazard; a green isolated family test cannot close it.
The existing transitional fallback fixtures expressly preserve this behavior.

**Disposition:** known and intentionally staged, not a newly introduced
defect. W6G.1f.3i must first remove state-bearing route payloads; W6G.1f.2b
then removes the coordinator lazy-producer machine and forces both orders of
the last-subscriber/first-session-close races. W6G.1e.2/e.3 subsequently
removes global producer selection. Do not declare W6G.1 implemented or use
stress repetition as repair evidence before those gates pass.

### W6GIR-002 — Medium audit gap: producer inventory checks names, not ownership shape

The enum still contains `Whnf(WhnfComputation)`, whereas migrated variants
are fieldless route markers. The source-backed inventory notices an added or
removed name but would accept a registered root or computation added as a
field to an existing marker. Its boundary-string check is likewise a
presence check, not a dataflow proof.

**Disposition:** W6G.1f.3i.0 should census every constructor and payload;
W6G.1f.3i.3-.4 should remove `Whnf`, assert fieldless remaining markers or
review their transient scalar exception, and reconcile the source/edge/root
inventories against actual types. This is an audit-strength finding, not
evidence that a migrated checkpoint presently contains an illicit root.

### W6GIR-003 — Medium verification gap: family tests precede coordinator cutover

Many forced family fixtures recreate `LazyTaskMachine` directly after dropping
the prior route. That proves checkpoint persistence and no-replay in the
family, but bypasses the coordinator's subscription count, session-affine
record, and claim-retirement race. The source-backed tests therefore cannot
establish that a *real* last subscriber releases the route or that a new
client/spark/reflection observer can reclaim the same checkpoint. The
existing first-discoverer failures in the parent plan make this gap material.

**Disposition:** carry to W6G.1f.2b and W6G.1f.4, as already partitioned.
Use latched orders for claim/publication/close/new-demand and distinguish
autonomous reflection roots from demand-backed producer routes. Representative
family fixtures are enough once their separate no-replay proofs are retained.

### W6GIR-004 — Low documentation drift: current-state descriptions lag both migrations

[`architecture/evaluation.md`](../architecture/evaluation.md) correctly
describes the ordinary WHNF checkpoint but still says all other specialized
lazy producer families are coordinator-owned. Their partial semantic state
has moved beneath typed managed checkpoints; only the temporary route machine
remains coordinator-owned. [`src/README.md`](../../src/README.md) also still
calls the durable rooted WHNF checkpoint "pending W6G.3 aggregation," although
W6G.3 has completed. Both wordings obscure the current representation.

**Disposition:** update the present-tense architecture paragraph and source
module map at W6G.1f.3i.4, keeping the temporary route machine explicit until
W6G.1f.2b retires it. Do not make current architecture claim the final pump
policy before W6G.1e/g/h implement it.

## Drift judgment and next gate

The reflection-to-promise path, pure `ListEffect` net construction, separate
source-entry seed, and aggregate rooted WHNF cell are purposeful refinements,
not deviations requiring reversal. The initial W6G.1 baseline's reflection
checkpoint classification was corrected by the design review and matches
current code. The later plan remains coherent in this order: close 3i;
perform 2b's route/session cutover and forced races; integrate retention and
collection in 4; then causal selectors, foreground lifecycle/drains, and
serialization retirement. W6G.5 remains the mandatory *final* review of all
W6G mechanisms and measured overhead. Its scope should not be narrowed on
the strength of this interim audit.
