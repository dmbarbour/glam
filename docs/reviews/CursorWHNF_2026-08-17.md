# Cursor WHNF Implementation Review — 2026-08-17

Baseline: `14e9dda55745`. This is a review-only audit of the completed
transition in [CursorWHNF.md](../.tmp/CursorWHNF.md) against the current
implementation and tests.

## Outcome

The implementation substantially realizes the intended cursor-WHNF boundary.
Demand is request-relative, cursor state has an authoritative owner, source
spines are transient, source active pairs remain ordinary interaction work,
and the evaluator uses an iterative exact-work driver rather than a whole-net
reducer. The former compatibility entry points are gone. The net-wide batch
lease and broad disturbance epoch are recognizable, documented performance
policies rather than hidden semantic shortcuts.

The high-severity locking defect and material terminal-observation mismatch
found by this review have since been fixed. Logical-copy installation now
captures the immutable source endpoint before acquiring the target net lock,
then carries it through a prepared copy-source token. A stale exact-pair
observation also propagates the pair's authoritative `Stuck` result rather
than allowing unrelated topology progress to postpone it.

The verification gap found by the review is now closed as well. The remaining
findings concern one read path which publishes a false mutation and small
post-transition representation cleanup. The review also records one resolved
clarification about recursive semantic evaluation versus cursor-work
ownership. None argues for restoring recursive cursor execution, a whole-net
fallback, topology demand agents, or reference-count-dependent semantics.

## Findings

### CW-001 — Resolved: logical-copy installation held source and target locks together

Confidence: high.

At the reviewed baseline,
[`RuntimeNet::begin_copy`](../../src/interaction_net/runtime/cursor.rs#L42)
called `source.with(RuntimeNet::exposed)` before installing the target-owned
`CopyState`. That was safe while constructing an unshared `RuntimeNet`, as
most low-level fixtures did. It was not safe through the production call path:
[`progress_exact_core_call`](../../src/eval/net.rs#L630) invokes
`runtime.with_mut(...)`, then
[`resume_claimed_call_with_copy`](../../src/interaction_net/runtime/cursor.rs#L72)
reaches `begin_copy` while the target's `SharedRuntimeNet` mutex is still held.

This creates two failures:

- if source and target are the same `SharedRuntimeNet`, the thread recursively
  locks a non-reentrant mutex and hangs;
- if two threads install copies from A into B and B into A, they can acquire
  the two mutexes in opposite order and deadlock.

The public API does not exclude the first shape. A
[`PromiseResolver`](../../src/api.rs#L396) accepts any value from the same
evaluation runtime, while [`NetBuilder::data`](../../src/api.rs#L548) accepts
that promise as net data. A promise may therefore be resolved with the net
which contains it. The known retained `Arc` cycle is separate from the
immediate mutex deadlock once the promised net is called.

Resolution on 2026-08-17:

1. [`SharedRuntimeNet::prepare_copy_source`](../../src/interaction_net/runtime/cursor.rs#L27)
   reads and retains the source exposed port before target mutation.
2. `RuntimeNet::begin_copy` and claimed-call completion now accept only the
   resulting `PreparedCopySource`; neither operation can inspect another
   shared net while the target lock is held.
3. [`progress_exact_core_call`](../../src/eval/net.rs#L630) prepares a
   net-valued callable before entering `runtime.with_mut(...)`.
4. `logical_copy_preparation_does_not_reenter_the_target_net_lock` exercises
   the former self-source deadlock directly, while
   `reciprocal_copy_installation_never_nests_runtime_net_locks` uses a barrier
   to hold both target locks before the two insertions proceed. Existing
   ordinary call and independent-copy fixtures remain passing.

Do not solve this with a lock order. The intended invariant is stronger and
cheaper: never hold two runtime-net locks simultaneously.

### CW-002 — Resolved: an unrelated topology revision invalidated a permanent stuck endpoint

Confidence: high.

The plan explicitly makes `Stuck` exceptional: once observed under the source
lock, the terminal pair cannot reduce, node IDs are not reused, and unrelated
source mutations cannot invalidate that terminal spine
([plan](../.tmp/CursorWHNF.md#transient-demand-spines-and-frontier-observations)).

At the reviewed baseline, the implementation did not preserve that exception. A
[`FrontierObservation`](../../src/interaction_net/runtime.rs#L182) retains the
pair and topology revision. Its `step_active_pair` passes the expected revision
to `step_active_pair_if_current`, which returned `Disturbed` on any mismatch
*before* inspecting `ActivePairState::Stuck`.

After a finite unrelated mutation the driver safely restarts, rescans, and
rediscovers the same failure. The error is not corrupted. The mismatch is
nevertheless observable as unnecessary dependence on unrelated work, and
continuing mutation can indefinitely postpone a permanent demanded error.
That is contrary to request-relative terminality even though the broad
revision remains acceptable for nonterminal endpoints.

Resolution on 2026-08-17:

1. On a topology-revision mismatch,
   [`step_active_pair_if_current`](../../src/interaction_net/runtime.rs#L744)
   performs one exact lookup. An authoritative `Stuck` state propagates
   immediately; every other stale state remains `Disturbed`.
2. Current-revision dispatch is unchanged. The added lookup occurs only on a
   path which would otherwise rebuild and rescan the frontier.
3. `nested_cursor_preserves_structured_failure_across_unrelated_source_progress`
   now captures the observation before an unrelated reduction and dispatches
   it afterward, preserving the structured failure.
4. `active_source_call_is_a_dependency_and_is_never_copied` now also verifies
   that an exact claimed dependency propagates its later terminal failure.
   Nonterminal stale-observation tests continue to require `Disturbed`.

### CW-003 — Resolved clarification: semantic recursion is not a cursor coordination cycle

The initial review conflated runtime identity re-entry with a cycle of cursor
work ownership. The transition's hierarchy claim is narrower and sound.

Materializing a logical copy creates target-local cursors into a closed source.
It does not create a cursor from that source back into the target, and the
driver retains no source claim while crossing runtimes. Cursor administration
therefore cannot create a cycle of mutually held claims.

Promises and fixpoints can still make a closed net contain itself as data.
Forcing that data may revisit the same shared runtime. This folds the unbounded
chain of partial evaluations which fresh instantiation of an equivalent closed
template would produce; it does not turn one independently materialized copy
into the source of its ancestor. Failure to reach a frontier is semantic
divergence or a lazy/promise dependency, not a cursor-claim deadlock.

The transition plan and current interaction-net contract now state this
distinction explicitly. No cursor-cycle detector or restriction on recursive
net values is recommended. The CW-001 repair remains independently necessary:
even a semantically divergent computation must not become a host mutex
deadlock while installing a copy.

### CW-004 — Resolved: Phase 5's final verification claim was broader than the tests

Confidence: high.

Phase 5C.3 required “deep stable and productive mixed cursor chains beyond the
former depth limit” and then recorded those cases as covered
([CursorWHNF.md](../.tmp/CursorWHNF.md#phase-5c3-final-request-relative-verification--complete)).
At the reviewed baseline, the implementation had good focused coverage but not
that complete matrix:

- [`iterative_cursor_driver_exceeds_the_former_recursion_limit`](../../src/eval/net.rs#L1122)
  covers 1,100 productive pairless copy layers;
- the stable pairless and mixed-owner tests at
  [`stable_cursor_dependencies_propagate_through_pairless_layers`](../../src/eval/net.rs#L834)
  and
  [`stable_cursor_dependencies_propagate_through_mixed_owner_layers`](../../src/eval/net.rs#L863)
  cover only two layers;
- there is no deep alternating pair-owned/pairless productive chain;
- the nested structured-failure test now dispatches a stale observation but
  does not propagate through the complete evaluator driver; and
- [`connecting_a_cursor_transfers_ready_and_blocked_obligations_to_the_pair`](../../src/interaction_net/runtime/tests.rs#L1247)
  omits stable-obligation transfer and rejection of an in-flight claimed
  transfer, even though the implementation handles both.

Resolution on 2026-08-17:

1. `deep_stable_cursor_dependencies_exceed_the_former_recursion_limit`
   propagates stable disposition through 1,100 pairless runtime layers.
2. `deep_productive_cursor_chain_alternates_pairless_and_pair_owned_layers`
   drives data through 1,100 alternating owner forms. Its transparent
   pair-owned fixture uses ordinary `Fan >< RemoteCursor` work rather than a
   test-only cursor transition.
3. `nested_terminal_failure_propagates_through_the_complete_driver` installs
   the exact nested dependency, disturbs the source with unrelated progress,
   then requires `NormalizationRequest::drive` to return the structured
   failure.
4. `connecting_a_cursor_transfers_ready_blocked_and_stable_obligations_to_the_pair`
   now covers all transferable states, and
   `connecting_a_cursor_rejects_transfer_of_a_claimed_obligation` latches the
   in-flight rejection.

No production behavior changed for this finding. The only supporting addition
is a `cfg(test)` constructor for the transparent pair-owned layer.

### CW-005 — Low: reading claimed call data publishes a graph mutation

Confidence: high.

[`RuntimeNet::claim_call`](../../src/interaction_net/runtime.rs#L1343) only
validates `ActivePairState::Claimed` and clones the immutable data payload. The
state transition to `Claimed` already occurred when the exact pair was reduced.
Nevertheless, `progress_exact_core_call` invokes it through unconditional
[`SharedRuntimeNet::with_mut`](../../src/interaction_net/runtime.rs#L592).
That increments the topology revision and either marks the current batch dirty
or publishes a disturbance.

The result is a false structural invalidation and potentially an unnecessary
follower wake every time callable lowering begins. It also needlessly makes
frontier observations stale. This is unnecessary work rather than a semantic
corruption.

Make `claim_call` take `&self` and call it through `SharedRuntimeNet::with`.
Add a revision test showing that reading the already claimed payload is quiet,
while completing, blocking, or failing the claim still advances authoritative
state.

### CW-006 — Low: two transition-era generalities no longer serve production

Confidence: high.

First, `FrontierObservation` retains `anchor` and the complete
`RuntimeNetRevisions`. Production dispatch uses only the source, endpoint, and
topology revision. The anchor is checked only while constructing the
dependency; on disturbance the driver discards transient work and reconstructs
the path from the authoritative parent cursor and request root. The stored
disturbance epoch is likewise unused; `NetContention` is the representation
which needs it.

Second, [`NetDriver::new`](../../src/eval/net.rs#L231) still accepts arbitrary
`NetDriverWork`, and `restart_from_request_root` can fail when no root exists.
Every production call and every full-driver test starts from
`RequestRoot`. Assertions compensate for a rootless state which the public
construction path cannot create.

Recommended cleanup:

- shrink `FrontierObservation` to source, topology revision, and endpoint
  unless the anchor is deliberately retained for future diagnostics; and
- construct `NetDriver` from `NormalizationRequest` or runtime/interface,
  make the root structural, and make restart infallible.

This is benign implementation drift: the authoritative parent state is a
better reconstruction owner than the observation itself. The plan and current
contract should be updated together if the fields are removed.

## Ownership and transition map

| Concern | Authoritative owner | Transition path |
| --- | --- | --- |
| Nodes, links, copies, active pairs | `RuntimeNet` under one `SharedRuntimeNet` mutex | exact graph helpers and `step_active_pair` |
| Pair-owned cursor state | `ActivePairState::{Ready, Claimed, BlockedCursor}` | claim under owner lock; inspect source unlocked; complete under owner lock |
| Pairless cursor state | `cursor_obligations: HashMap<NodeId, PairlessCursorObligation>` | root demand installs; step claims; finish blocks, stabilizes, or removes with cursor |
| Copy provenance | target-local `CopyState { source, frontiers, fan_sites }` | begin copy, principal materialization, local convergence |
| Transient source work | `FrontierObservation` and `DemandEndpoint` | inspect under source lock; version-validate one exact step |
| Parent resumption | `NetDriverWork::ResumeCursorDependency` | exact dependency equality, then owner-local Ready or Stable publication |
| Root demand | evaluator-local `NormalizationRequest` | locked `poll_interface_demand`, then iterative worklist |
| Contention | `NetContention { runtime, revisions }` | wait on disturbance epoch, then rebuild from root |
| Notification batching | net-local `NormalizationBatchState` plus weak RAII lease | topology revision per mutation; disturbance on batch close |
| Semantic external wait | `ActivePairState::BlockedCall` / `BlockedOperatorCall` | exact `EvaluationWaitToken`, scheduler-visible `EvaluationHalt::blocked` |

The normal production trace is:

```text
NormalizationRequest
  -> locked root poll
  -> exact cursor or active-pair work
  -> child dependency before parent resumption
  -> owner-local dependency resolution
  -> rebuild and re-poll the request root
  -> Data | Bind | other normal form | stable cursor | wait | stuck failure
```

The old `demand_interface`, `claim_dependent_cursor`, `claimed_cursors`,
`FrontierObservationStatus`, and whole-runtime evaluator scheduling paths have
no surviving source reference. `RuntimeNet::reduce_next` remains deliberately
as the generic reducer and low-level test utility; cursor-WHNF production uses
`reduce_pair` on exact selected work.

## Invariant accounting

| Intended invariant | Accounting |
| --- | --- |
| Preserve source work sharing | Implemented. A logical copy stores its shared source and materializes only stable principal-frontier nodes. |
| Normalize a cursor in its owning runtime | Implemented for cursor transitions and dependency resolution. Copy creation prepares the source endpoint before entering the target owner. |
| Keep cursor administration distinct from interaction rules | Implemented. `CursorStep` and `ActivePairStep` keep discovery/administration explicit, while exact source pairs use ordinary `reduce_pair`. |
| No demand agents or refcount policy in topology | Implemented. Demand resides in evaluator work and owner-local obligation records. |
| Materialize only from a source principal frontier | Implemented in `inspect_source_frontier_shape` and `materialize_remote_node`. |
| Auxiliary demand discovers rather than copies active work | Implemented. Traversal follows principal links to an endpoint and copies no auxiliary-entered node. |
| Converging frontiers do not duplicate materialization | Implemented through `CopyState::frontiers`; an in-flight peer is retained until its claim finishes. |
| Independent logical copies remain independent | Implemented. Frontier and fan-site maps are target-copy-local; the source computation remains shared. |
| Exactly one owner per cursor transition | Implemented and asserted: active-pair owner XOR pairless obligation owner. Authority transfer is under the target lock and tests cover ready, blocked, stable, and rejected claimed transfer. |
| Do not retain complete demand spines | Implemented. `principal_anchors` exists only during one locked inspection and is discarded after convergence lookup. |
| Durable endpoint work is version validated | Implemented for nonterminal cursor/pair observations. An exact pair's authoritative `Stuck` result is terminal despite unrelated revision changes (CW-002 resolution). |
| Never hold two net locks | Implemented by cursor advancement and copy installation. `PreparedCopySource` separates source inspection from target mutation (CW-001 resolution). |
| No lost wakeup around claimed work | Implemented by capture-under-lock, disturbance epoch recheck under the same mutex, and condition-variable waiting. Barrier tests cover the disputed ordering. |
| Batch follower disturbance without per-step thrashing | Implemented with a net-wide RAII lease. Every topology mutation advances its revision; only batch close publishes disturbance. |
| Request-relative completion | Implemented for root shapes, stable cursors, unrelated active work, exact waits, and demanded failures, including stale observations of terminal pairs. |
| Iterative traversal with no 1,024-layer limit | Implemented and proven independently for 1,100-layer productive pairless, stable pairless, and alternating productive pairless/pair-owned chains. |
| Raw `Value::Net` is opaque WHNF | Implemented. Explicit net computation and arity/function bridges initiate normalization; ordinary value evaluation does not. |

## Drift ledger

### Intentional refinements

- The locked root poll is conditionally mutating: its first observation of a
  pairless cursor installs the authoritative obligation. Repeated polls are
  quiet. This is clearer than separating observation from demand admission.
- Stable child completion is propagated as a `CursorDependencyDisposition`
  through the same exact parent record instead of retrying and rediscovering a
  stable child.
- The worklist stores only immediate child work and parent resumptions. It does
  not retain the source traversal spine.
- A directly selected root active pair uses the live `ActivePairKey` without a
  separate revision snapshot. Ordinary reduction removes its nodes, node IDs
  are not reused, and pair-key-preserving cursor administration retains the
  demanded anchor; the exact step still validates current pair state.
- `RuntimeNet::reduce_next` remains available below the evaluator boundary.
  This is a useful generic primitive, not a surviving cursor-WHNF fallback.
- Recursive semantic evaluation may revisit a shared closed-net runtime, but
  cursor materialization still creates no cycle of mutually held work claims.
  CW-003 records the clarified scope of the hierarchy invariant.
- Logical-copy installation uses a prepared source token so the target-owned
  mutation phase has no reason or ability to acquire the source net lock.

### Accidental or unresolved drift

- A read-only call-payload operation publishes mutation (CW-005).
- Observation and driver representations retain now-unused generality
  (CW-006).

### Current documentation drift

The focused current contract says that “shared runtime mutation increments a
condition-variable generation” in
[`interaction_nets.md`](../agent_context/interaction_nets.md#core-specialization).
The implementation now has two generations: topology revision advances for
every authoritative mutation, while disturbance epoch and notification may be
deferred until the normalization batch closes. Update that paragraph when
CW-005 is fixed so it describes the authoritative publication rule rather than
the pre-batching mechanism.

### Deliberately deferred policy

- net-wide rather than per-frontier batch ownership;
- broad disturbance instead of exact subscriber indexes;
- condition-variable parking rather than scheduler-visible net suspension;
- no cached spine frames, propagated frontier revisions, or reduction quotas;
- no SNF-like, annotation-guided, or JIT normalization policy; and
- no public exposure of normalization descriptors or bootstrap cursor jargon.

These limits are visible in the code and do not currently alter the intended
language semantics. Net-wide serialization can delay independent work, but it
was explicitly chosen to prevent synchronized followers from thrashing after
every reduction.

## Test evidence

| Area | Direct evidence | Assessment |
| --- | --- | --- |
| Root shapes and idempotence | `conditional_runtime_mutation_publishes_only_new_cursor_obligations`, `root_normalization_demand_is_idempotent_and_enumerable`, `interface_demand_poll_classifies_stable_roots_and_exact_work` | Strong structural coverage. |
| Stale root selection | `interface_demand_work_selection_is_revalidated_before_dispatch` | Exact stale selection is covered. |
| Owner forms and dependency resolution | `cursor_dependency_resolution_updates_both_owner_forms`, `cursor_dependency_resolution_rejects_stale_or_missing_parents_without_mutation` | Good positive and negative coverage. |
| Pairless lifecycle | `pairless_cursor_obligation_transitions_have_one_owner`, `removing_a_cursor_removes_its_dormant_obligation` | Good owner and cleanup coverage. |
| Authority transfer | `connecting_a_cursor_transfers_ready_blocked_and_stable_obligations_to_the_pair`, `connecting_a_cursor_rejects_transfer_of_a_claimed_obligation` | Complete transferable-state coverage plus in-flight rejection. |
| Nonblocking step states | `cursor_steps_report_pairless_pair_owned_stable_and_contended_states`, `active_pair_steps_report_reduction_contention_blockage_stuck_and_gone` | Broad enumeration coverage. |
| Source/target lock separation | `remote_cursor_exposes_source_progress_without_holding_nested_locks`, `root_cursor_claim_remains_exclusive_while_source_inspection_is_in_flight`, `logical_copy_preparation_does_not_reenter_the_target_net_lock`, `reciprocal_copy_installation_never_nests_runtime_net_locks` | Strong for both advancement and logical-copy creation. The reciprocal case forces both target locks to be held before insertion. |
| Inspect/publish/wait race | `source_change_between_cursor_inspection_publication_and_wait_is_not_lost` | Barrier-driven and appropriately forced. |
| Parallel pairless demand | `concurrent_interface_demands_share_one_pairless_cursor_claim` | Barrier-driven ownership coverage. |
| Batch lifecycle and followers | `normalization_batch_lease_is_exclusive_and_drop_safe`, `normalization_batch_defers_disturbance_until_release`, `normalization_batch_wakes_a_registered_follower_once_at_release` | Strong, including RAII release and one closing wake. |
| Active source work is not copied | `active_source_call_is_a_dependency_and_is_never_copied`, auxiliary-chain tests | Directly covers the topology/work split. |
| Transitive cursors and reinspection | `layered_cursor_reports_and_follows_an_exact_dependency`, `nested_cursor_demand_reuses_a_claimed_source_obligation`, `auxiliary_cursor_recomputes_its_spine_after_each_terminal_pair`, `auxiliary_cursor_reinspects_after_a_principal_remote_cursor_materializes` | Strong shallow semantic coverage. |
| Two-sided convergence | `converging_frontiers_join_without_leaving_a_stale_cursor_pair`, `converging_frontier_waits_for_a_claimed_peer` | Covers duplicate prevention and in-flight peer behavior. |
| Independent copies | `separate_logical_copies_rebase_fans_to_distinct_local_sites` | Proves copy-local fan identity and source sharing. |
| Request-relative unrelated work | `stable_root_does_not_reduce_disconnected_or_undemanded_ready_work`, `stable_root_ignores_unrelated_claimed_and_stuck_work` | Good controls for global-work leakage. |
| Exact semantic waits | `blocked_call_requires_its_current_wait_token_to_be_reclaimed`, operator equivalent, reflection-gate call/operator tests | Exact token and scheduler-visible retry are covered. |
| Permanent failure | generic stuck tests, `specialization_failure_remains_structured_in_the_stuck_pair`, `nested_cursor_preserves_structured_failure_across_unrelated_source_progress`, `nested_terminal_failure_propagates_through_the_complete_driver`, and the terminal part of `active_source_call_is_a_dependency_and_is_never_copied` | Direct roots, stale exact-observation dispatch, and full nested driver propagation are covered. |
| Iterative depth | `iterative_cursor_driver_exceeds_the_former_recursion_limit`, `deep_stable_cursor_dependencies_exceed_the_former_recursion_limit`, `deep_productive_cursor_chain_alternates_pairless_and_pair_owned_layers` | Complete pairless productive, stable, and mixed-owner matrix beyond the former bound. |
| Runtime transfer | `cursor_driver_releases_each_runtime_before_crossing_to_the_next` | Proves leases are closed before crossing between runtime nets. Copy installation lock separation is covered independently above. |
| Public semantic boundary | raw-net opacity, `net_arity`, net computation/function, non-data-normal-form, and executable sample tests | Good language-level integration coverage. |

The focused runtime suite contains 63 tests and the evaluator driver suite 13;
both pass after the CW-001, CW-002, and CW-004 resolutions. The full repository
suite passes with 1,276 tests across all targets.

## Recommended order

1. Remove the false call-read publication (CW-005), then perform the small
   observation/driver and current-doc cleanup (CW-006).

After these items, the cursor-WHNF transition can reasonably be treated as a
closed correctness foundation. Further changes should be driven by profiling
of batch duration, repeated spine scans, disturbance breadth, and parked worker
time rather than by speculative scheduler machinery.
