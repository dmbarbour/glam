# Glam GC Integration Phase I3 Review — 2026-09-02

Baseline: `5055c23`, including completed Phases I3A-I3F. Review corrections
are recorded below.

Status: complete. Phase I3 establishes bounded, runtime-qualified managed
authority throughout evaluation, workers, effects, interaction nets,
compilation, diagnostics, and host callbacks while production collection
remains disabled. All review findings are resolved; no finding blocks I4.

## Scope

This is the mandatory post-implementation review for integration Phase I3. It
audits the implementation against:

- the I3 requirements in
  [`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md);
- the scoped-quantum and subsystem inventory resolution in
  [`GarbageCollectorIntegration_2026-08-25.md`](GarbageCollectorIntegration_2026-08-25.md);
- the mutator, ownership, and production-collection gates in
  [`GarbageCollectionRoadmap_2026-08-19.md`](../plans/GarbageCollectionRoadmap_2026-08-19.md);
- the transient/rooted owner records in
  [`GarbageCollectorOwnershipLedger_2026-08-20.md`](../plans/GarbageCollectorOwnershipLedger_2026-08-20.md); and
- the participant-entry successor requirements in
  [`ConcurrentGarbageCollection_2026-08-28.md`](../plans/ConcurrentGarbageCollection_2026-08-28.md).

The review asks whether I3 supplies a coherent authority protocol before
managed semantic pointers arrive, whether every wait and callback ends active
managed access, whether durable boundaries carry roots rather than scoped
borrows, and whether source/schedule tests close the implemented surface. It
does not certify a production `Trace`, convert `RuntimeValueRoot` to a
collector root, or authorize production collection. Those remain I4 and later
gate work.

## Outcome

The implemented boundary matches the intended three-way distinction among
semantic purity, evaluator purity, and operational callback-freedom:

- `EvaluationPollContext` owns checked orchestration authority but no mutator;
- `EvaluatorStepContext` is thread-bound and may span evaluator orchestration,
  but opens managed access only through a higher-ranked callback;
- `EvaluationValueAccess` contains the runtime-qualified mutator region and
  cannot escape, become `Send`, or become `Sync`;
- direct synchronous evaluation retains one source-latched internal admission
  gateway, while scheduled task, client-demand, and spark polls derive their
  context from a checked demand claim;
- machine completion and terminal wait state carry `RuntimeValueRoot`, not a
  bare value reconstructed by the scheduler;
- ordinary waits, pumping, callbacks, task release, cancellation,
  terminalization, and worker sleep run after managed access ends;
- reflection activation is reserved during evaluation and activated only
  after the evaluator carrier is dropped;
- effect evaluation and host interpretation alternate explicit phases, with
  standard-effect fusion checked against the unfused semantic reference;
- core-net observation and mutation require an exact matching runtime access,
  and callable, operator, cursor, normalization, and contention work use
  bracketed claims which terminalize before publication or parking;
- semantic thunks receive scoped evaluator authority, while deterministic host
  calls receive no evaluator carrier and return a runtime root;
- compiler, macro, diagnostic, event, rendering, and executable callbacks
  retain roots or owned host data across waits and host boundaries; and
- nested runtimes use independent heap-qualified TLS records. Worker exit
  explicitly retires inactive cache records, while ordinary quantums preserve
  reusable cursors.

Production runtime heaps remain `CollectionPolicy::NoAuto`, and production
`RuntimeValueRoot` still contains the compatibility `core::Value`. I3 proves
where future managed access may occur; it does not make current values traced
or rooted in the collector.

## Authority and Lifetime Accounting

There are three durable-to-ephemeral transitions:

```text
EvalContext / EvaluationDemandState
  -> EvaluationPollContext             no mutator
  -> EvaluatorStepContext               no mutator, thread-bound
  -> EvaluationValueAccess<'scope>      exact runtime mutator region
```

The final transition is higher-ranked, so neither the access carrier, its
mutator, an allocator, nor a managed borrow can be returned from the callback.
The collector's compile-fail contracts provide the underlying mutator and
allocator non-escape proof; Glam's negative trait fixtures additionally prove
that the evaluator carriers cannot move between threads.

`CoreValueFactory::with_runtime_value_access` is the only production access
gateway and delegates to the value domain's heap. The I3F inventory accounts
for every call by source owner and separately requires all direct
`Heap::with_mutator` calls to remain in `core::managed`. The construction-only
`with_managed_values` gateway is inventoried as well. Future I4 operations
which allocate, root, or borrow managed values are narrowly allowed on
`RuntimeValueAccess`; obsolete phase-wide `dead_code` suppression was removed
during this review.

The centralized direct evaluator gateway is intentional. It supports internal
synchronous entry from a caller which already owns an `EvalContext`; it opens
no ambient mutator and all recursive access still passes through bounded
callbacks. External production modules no longer call the durable-context
evaluator API directly. Scheduled work does not use this gateway.

## Poll, Wait, and Root Accounting

Every type-erased `EvaluationTaskMachine::poll` receives an
`EvaluationPollContext`. Task, deferred, client-demand, and spark paths share
the coordinator pump rather than implementing worker-specific access. A
claimed demand session is checked before detachment and retained only on the
orchestration stack.

`EvaluationMachinePoll::Complete`, error exits, terminal waits, task status,
and client-demand completion carry runtime-qualified roots. Evaluator
consumers project those roots only inside a bounded value-access callback;
non-evaluator consumers transfer the root directly. The compatibility root's
large inline payload is why `EvaluationWaitPoll::Complete` remains boxed and
statically limited to two machine words until I4F.2.

Forced schedules cover budget exhaustion, patient waits, another worker
owning the producer, blocked machines, terminal machine destruction, and
nested dependency work. The one special interaction-net disturbance wait is
not a general semantic wait: it holds a bracketed local net claim and waits for
another evaluator performing the same callback-free work. The cursor and
contention suites force publication-before-park and completion-before-
subscription orderings rather than relying on repetition.

## Callback and Subsystem Accounting

Reflection gates reserve a stable task identity inside evaluation and defer
activation until `EvaluatorStepContext::finish`, after managed access has
ended. Effect machines split request evaluation, host interpretation, and
continuation evaluation. Callback probes force `snapshot`, `commit`, and
specialization calls and verify collection can begin at each host boundary.

Interaction-net semantic locking is behind `CoreRuntimeNetAccess`, which is
constructed only from matching `RuntimeValueAccess`. Normalization and active-
pair/cursor claims are private guards with explicit dispositions; unwind
restores a safe ready state. Cross-net cursor work releases one net's scoped
access before entering another. I8 may therefore rely on ordinary semantic-net
lock holders possessing matching mutator authority, but must still introduce
the managed outer node, exact trace, mutation gateways, and durable root shape.

I3E separated `SemanticThunk` from `HostCallProducer`. The former is
callback-free evaluator work; the latter performs filesystem/import or other
host work in a mutator-free phase and publishes a runtime root. This separation
does not make captured closure environments traceable. I4B/I10 still must
replace or contain every raw value capture before production collection.

Compiler caches publish complete rooted bundles atomically. Macro state,
module declarations, imports, diagnostics, event payloads, rendering inputs,
and terminal output bytes cross callbacks as roots or owned Rust data. Source
inventories account for compiler roots/projections, lazy producer roles,
effect/net direct-entry closure, public compatibility accesses, and all
managed gateways.

## Multi-Runtime and Concurrent-Collector Accounting

Recursive same-heap access reuses the collector's TLS depth and cache. Nested
different-runtime access creates a separate heap-qualified record; runtime IDs
remain semantic provenance rather than collector authority. The collector's
barrier-controlled reciprocal A-then-B/B-then-A fixture proves that two
uncommitted stop-the-world requests do not impose a heap lock order.

This is a reference-collector entry proof, not a starvation-freedom proof.
The concurrent collector plan now explicitly re-runs I3F's admission,
mutator-free poll/wait, worker-exit, and source-inventory fixtures. CG0 adds
continuously overlapping-mutator schedules; CG1 reinterprets bounded access as
participant epochs and worker cache retirement as participant retirement.

## Drift Assessment

### Intentional and justified

1. **Production values remain unmanaged and collection remains `NoAuto`.** I3
   establishes authority before I4 introduces managed semantic pointers.
2. **One internal direct synchronous evaluator gateway remains.** It carries
   no mutator and is source-latched; external subsystem entries were removed.
3. **Semantic thunks and host calls are separate producer types.** This makes
   operational callback-freedom explicit without claiming closure traceability.
4. **Standard effects may fuse.** The unfused path remains available in tests,
   and equivalence/order/retry fixtures guard the optimization.
5. **Interaction-net disturbance waiting is a narrow structural exception.**
   Bracketed claim ownership and acyclic cursor work justify it; arbitrary
   waits with managed access remain forbidden.
6. **Worker quantum exit preserves TLS cursors.** Only worker termination
   retires inactive thread caches. Full collection recovers forgotten ranges.
7. **The concurrent collector receives proofs rather than current behavior.**
   I3 does not claim participant epochs or progress under overlapping mutators.

### Corrective new information

1. **Callable net data needed explicit function expansion.** The intervening
   function-call repair restores partial application by lowering a data-held
   function directly into the operator graph; it does not weaken net claim or
   authority boundaries.
2. **Client-demand result projection is owner-driven polling.** The review
   removed a second direct evaluator constructor and projects the completed
   root through `EvaluationPollContext::for_context`.
3. **Worker termination is a collector lifecycle boundary.** I3F added an exit
   guard which explicitly retires inactive per-heap caches.

### Accidental or convenience-driven drift

None remains after the findings below were resolved.

## Review Findings

### I3R-001 — The single direct-admission latch scanned only its expected owner

**Classification:** verification gap

**Status:** resolved

`direct_evaluator_admission_has_one_internal_compatibility_gate` read only
`src/eval.rs`, so it could prove the expected constructor existed but could
not prove another module had not constructed a direct evaluator context.
`EvalContext::evaluate_whnf` had done exactly that solely to project a
completed client-demand root. The projection was bounded and did not retain a
mutator, but it contradicted the single-gate architecture.

The test was first widened to scan the complete Rust source tree and failed
with the exact unexpected owner:

```text
left:  {"src/eval.rs": 1, "src/evaluation/session.rs": 1}
right: {"src/eval.rs": 1}
```

Result projection now uses the explicit owner-driven poll context. The widened
latch passes and will reject any future constructor outside `src/eval.rs`.

### I3R-002 — Transitional dead-code allowances outlived I3

**Classification:** implementation cleanup and verification hardening

**Status:** resolved

The complete `evaluation::access` module, `RuntimeValueAccess`, and its access
gateway retained broad allowances whose reasons said I3B had not yet migrated
the evaluator. Removing them exposed two production-unused conveniences:
`EvaluationValueAccess::values` and `EvaluationPollContext::root_value`.

Both conveniences are needed only by fixtures and are now `cfg(test)`. Broad
allowances were removed. The three `RuntimeValueAccess` allocation/root/borrow
operations which intentionally await I4 retain narrow operation-local
allowances naming their actual future owner. Strict Clippy therefore protects
the completed I3 authority surface instead of suppressing dead code across it.

### I3R-003 — Durable status and ownership indexes stopped mid-I3

**Classification:** documentation and verification-index drift

**Status:** resolved

The integration plan status omitted I3E.3/I3F and still marked I3 in progress;
the roadmap stopped during I3B; and the ownership ledger named only the first
I3 authority checkpoints while its Gate G2 blocker still said compiler/macro
structures needed I3 bounds.

The plans now record completed, reviewed I3. The ledger adds the I3 regression
matrix and states the remaining compiler/macro obligation precisely: I3 bounds
are complete, while I4F must still convert compatibility roots and bounded raw
values before managed production values appear.

## Verification

| Boundary | Evidence |
| --- | --- |
| thread-bound, non-escaping authority | collector compile-fail mutator/allocator/borrow fixtures; negative `Send`/`Sync` fixtures for `RuntimeValueAccess`, `EvaluationValueAccess`, and `EvaluatorStepContext` |
| bounded managed gateways | `all_managed_entries_have_bounded_mutator_regions`; `recursive_construction_reuses_one_mutator`; `poll_context_without_scope_carries_no_heap_authority` |
| centralized direct admission | widened `direct_evaluator_admission_has_one_internal_compatibility_gate`; `direct_evaluator_compatibility_entries_are_complete` |
| poll outcome ownership | `evaluation_machine_poll_boundary_inventory_is_complete`; root survival/release tests across poll and wait boundaries |
| waits, worker sleep, and destruction | `blocked_machine_parks_without_mutator`; `terminal_machine_destruction_occurs_without_mutator_authority`; `worker_releases_mutator_before_sleep`; `patient_claimed_task_wait_releases_mutator`; `scheduled_nested_dependency_runs_without_mutator` |
| reflection and effect callbacks | `reflection_gate_reserves_inside_and_activates_outside_scope`; forced activation/cancellation schedules; `effect_interpreter_callbacks_do_not_inherit_evaluator_mutators`; fused/unfused equivalence suite |
| interaction-net scope and claims | core-net foreign-domain/privacy inventory; callable/operator/cursor disposition and unwind suites; contention subscription-order fixtures; `cursor_driver_releases_each_runtime_before_crossing_to_the_next` |
| producer classification | `lazy_producer_roles_are_explicit_and_complete`; semantic-thunk and host-call callback/foreign-root fixtures |
| compiler and macro roots | `compiler_suspension_parks_only_roots`; `compiler_cache_publishes_complete_rooted_bundle`; `compiler_root_and_projection_inventory_is_complete`; macro/import forced schedules |
| event, diagnostic, and output callbacks | `event_delivery_invokes_callback_without_mutator`; output conversion/delivery forced-order fixtures; `diagnostic_rendering_invokes_writer_without_mutator` |
| multi-runtime TLS and worker exit | collector `reciprocal_nested_entries_pass_two_uncommitted_collection_requests`; `runtime_tls_caches_remain_heap_qualified`; `worker_termination_releases_inactive_collector_caches` |
| repository behavior | formatting, warnings-denied all-target/all-feature Clippy, complete root-package test suite, and focused collector reciprocal-entry fixture |

No race conclusion rests on repeated execution. The concurrency evidence above
uses barriers, latches, subscription hooks, or explicit state inspection to
force the relevant orderings.

## Later-Phase Review

I4 remains correctly ordered after I3:

- I4.0 must establish passive managed-destruction admission before any closed
  managed family fixture collects;
- I4A-I4E may use `RuntimeValueAccess` for isolated managed allocation and
  exact observation but may not publish production managed values;
- I4B must close closure and opaque hidden-edge boundaries left deliberately
  unresolved by I3E;
- I4F.1 must convert every durable owner before the representation switch;
  I3's bounded local proof is not a substitute for a registered root; and
- I4F.2 must switch the managed value shell and public/runtime root atomically,
  close compatibility projections, and retain matching-runtime authority.

No repartitioning is required before I4.0. I4F remains deliberately large but
already has the F.1 durable-owner gate before the F.2 atomic representation
switch; it should be reviewed again after I4A-I4E reveal the concrete managed
shell and family taxonomy.

## Decision

Phase I3 is complete. It supplies the bounded managed-authority and callback
protocol required by later production managed pointers, with source-backed
closure over every current heap entry and deterministic schedules at semantic
wait/callback boundaries.

Phase I4.0 may begin. This decision does not authorize a production `Trace`,
managed semantic representation, registered production root, explicit
production full collection, automatic collection, or concurrent-collector
progress. I4 and Gates G2/G3 retain those obligations.
