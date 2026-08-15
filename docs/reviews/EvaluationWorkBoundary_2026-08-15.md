# Evaluation Work Boundary Review — 2026-08-15

## Status and Scope

This is the compact historical record of the completed Evaluation Work
Boundary transition and its final audit. It is not required current context.
Current ownership and lifecycle are documented in:

- [`../architecture/evaluation.md`](../architecture/evaluation.md);
- [`../architecture/reflection.md`](../architecture/reflection.md);
- [`../architecture/diagnostics.md`](../architecture/diagnostics.md); and
- [`../architecture/assembly.md`](../architecture/assembly.md).

The review baseline was revision
`3c807bb9fbb7d7c3a242a64156b362aac675ef61` on 2026-08-14. The full temporary
transition plan, review procedure, section audits, and verification matrices
were retired after their surviving decisions were reconciled with permanent
architecture and invariants.

## Final Conclusion

The transition met its purpose. One `EvaluationRuntime` is the value,
reflection-heap, protected-volume, event, ID, cache, work-coordination, and
executor-attachment universe. Demand and reasoning sessions select ownership
and role policy within that universe rather than introducing separate state
universes.

Reproducible computation cannot inspect the reflection heap, while reflection
annotations demanded from it still use the runtime-default task profile and
shared heap. Exact completion subscriptions, broad semantic observation,
runtime mutation admission, readiness, settlement, retained reporting, and
fallback rendering now form one coherent lifecycle.

The audit found no Critical or High issue and no contradiction requiring a new
architecture. Twelve Medium or Low findings were resolved. One broad claim
retains the appropriate limitation: escaped handle and settled-report root
retention is supported by static ownership analysis and targeted teardown
tests, not exhaustive enumeration of every possible public value graph.

## Resolution Ledger

| ID | Baseline finding | Final resolution and focused evidence |
| --- | --- | --- |
| EWB-001 | Current docs retained session-owned task registries, logger shims, and incomplete Phase 10 prose | Reconciled runtime/session/logger/macro/volume ownership and completed lifecycle terminology |
| EWB-002 | Public API exposed raw `RuntimeSharedResources` transaction/query substrate | Made raw resources crate-private; external hosts receive constructed `RuntimeTaskCapability`; public compile-fail doctest |
| EWB-003 | Same-runtime other-session producer was called “foreign” | Reserved “foreign” for runtime mismatch; macro unavailable-producer regression |
| EWB-004 | Closed compiler and formatter cache builders registered transient scheduler demand | Added private unattached reduction context; `closed_runtime_cache_builders_do_not_register_scheduler_demand` |
| EWB-005 | No direct ordinary-result equivalence test across worker counts | `ordinary_result_is_identical_across_worker_counts` covers zero, one, and multiple workers |
| EWB-006 | Client-demand retirement published its sink and dropped values while mutation admission remained held | Detached state under admission, then published/dropped after release; `client_demand_retirement_publishes_after_runtime_unlock` |
| EWB-007 | Task terminalization published obligations through several independent mutation guards | One guarded terminal inventory covers wait/status, failure policy, and task-owned promises; `task_terminal_surfaces_publish_under_one_mutation_admission` |
| EWB-008 | Reflection-only no-op commit advanced semantic observation epoch | Store commit preserves a changed bit; no-op preserves epoch while query retirement still publishes; two focused commit regressions |
| EWB-009 | Diagnostic ingress constructed/rooted transport beneath mutation admission | Prepared the runtime root first; ingress preparation failure remains counted and reportable |
| EWB-010 | Logger diagnostic route and replacement root activated in separate settlement-visible transitions | Prepared root in `Reserved`, then activated route and root beneath one exclusive guard; forced-order regression |
| EWB-011 | Executor shutdown lacked a barrier test while a worker owned task/promise work | `executor_shutdown_preserves_worker_owned_cancellation_and_task_promise` |
| EWB-012 | Same-session ready-task ordering lacked direct coverage | `serial_ready_tasks_preserve_same_session_fifo_order_across_requeue` |

The terminal-publication work also exposed and fixed two related orderings:

- nonterminal reflection release now publishes status before a racing demand
  owner can close the session; and
- the configured logger recursion fixture now commits each read before
  recurring, rather than strictly observing an unfinished monadic fixpoint.

Both were latched with forced ordering before repetition and full-suite checks.

## Accepted Representation Refinements

These differences from preliminary plan sketches are deliberate current design,
not incomplete transitions.

### Runtime profile and resources are sibling roots

`EvaluationRuntime` owns lifecycle `RuntimeState` and its immutable default
`ReflectionTaskProfile` as sibling roots. Runtime-backed hosts retain the
acyclic shared resource bundle, while the resource bundle retains only a weak
coordinator route. This avoids
`state -> profile -> launcher -> host -> state` without placing either root
outside the runtime boundary. It remains compatible with a future runtime
arena or tracing collector.

### Work blocks use kind-local payloads

One coordinator `WorkRecord` is authoritative, but reflection, deferred,
spark, and client-demand kinds retain only their applicable machine, block,
exit, and demand data. This excludes invalid combinations more directly than a
single large `WorkBlock` payload while preserving common identity, control,
subscription epoch, and settlement obligations.

### Store and event state share the transaction mutex

The reflection store and runtime event journal commit through one transaction
state mutex, giving atomic heap edits, admitted-input claims, and buffered
outputs. Store and event revisions remain separate conflict domains. The outer
mutation-admission gate makes sequential observation-epoch publication safe
without nesting component mutexes.

### Compiler cache attachments publish complete winners

Runtime cache extensions are type-indexed. Complete bundles are built outside
the cache mutex and one winner is installed; concurrent first users may perform
harmless duplicate pure construction. The g-compiler bundle has a secondary
on-demand cache for arbitrary effect paths, also publishing only completed
values.

### Diagnostic reorder state is separate from transactional route state

Ingress sequence reordering is not transaction-visible and therefore uses its
own lock. Runtime route and FIFO state remain in the event transaction domain.
`RuntimeState` owns the stable ingress identity, while ingress back-references
to runtime resources are weak.

### Promise obligations install directly in running records

A promise created during a running quantum enters that authoritative
coordinator record beneath shared admission and the coordinator lock. The
record is already `Busy`, so terminalization cannot miss an unpublished
worker-local obligation.

## Final Ownership Summary

```text
EvaluationRuntime
  -> RuntimeState
       -> EvaluationExecutor
       -> EvaluationWorkCoordinator
       -> RuntimeSharedResources
       -> stable DiagnosticIngress registrations
  -> immutable default ReflectionTaskProfile

RuntimeSharedResources
  -> value factory/cache, transaction state, observation epoch,
     runtime IDs, mutation admission
  -weak-> EvaluationWorkCoordinator

EvaluationSession owner lease
  -> EvaluationDemandState
  -> coordinator
EvaluationDemandState
  -weak-> coordinator

coordinator work records
  -> claimable machines, rooted payloads, task/wait indexes,
     exact dependencies, terminal obligations, failure ledgers
  never -> EvaluationSession owner lease
```

Public `Value`s and retained event/task payloads carry runtime-local roots.
Task handles retain their terminal wait, status-query lease, scalar ownership
identity, and weak coordinator route, not the demand owner. Same-runtime
sessions may observe and control those handles; another runtime is rejected at
the value boundary.

## Locking and Publication Result

- Runtime mutation and state-changing coordinator commits take shared mutation
  admission. Readiness and settlement take it exclusively.
- Hold at most one component mutex at a time. Update the component, release its
  mutex, advance semantic observation while admission remains held, then
  release admission.
- Completion sources initialize terminal state before detaching exact
  subscribers. Coordinator requeue validates work ID, subscription epoch, and
  dependency identity.
- Combined reflection/event commit publishes both journals or neither.
- Output delivery claims and terminalizes identified outbox records under
  admission, but performs decoding and callbacks outside locks.
- Settlement validates coordinator, transaction, observation, diagnostic, and
  delivery activity under one exclusive interval before publishing terminal
  dispositions and reporting obligations.
- Wakes, sinks, subscribers, callbacks, value destruction, and machine
  destruction occur after component locks and mutation admission are released.

## Deferred Work

The completed bootstrap intentionally does not expose partial versions of:

- evaluator fuel or preemption for divergent worker quanta;
- fine-grained heap/event wake subscriptions beyond one broad semantic epoch;
- automatic cycle poisoning through promises or reflection tasks;
- work priorities beyond current FIFO/class fairness;
- executor sharing across runtimes, remote/cloud work transport, or IPC values;
- `.task.new_with` alternative profile selection;
- stages, machine migration, or persistent services surviving demand-owner
  closure;
- transactional sparks;
- general external transactions beyond admitted input and buffered output; or
- arena/tracing GC for recursive `Arc` values and persistent containers.

The seams are deliberate: centralized runtime work and dependency identity,
runtime-rooted values, explicit demand owners, exact/broad subscription split,
and a narrow public host capability leave those extensions possible without
pretending they already exist.

## Verification

The final resolution state passed:

```text
cargo fmt --check                                         PASS
cargo clippy --all-targets --all-features -- -D warnings PASS
cargo test -q                                             PASS
  1097 library tests
  22 binary tests
  49 CLI tests
  all remaining integration and sample suites
cargo test --doc -q                                       PASS
```

Concurrency findings used explicit barriers to reproduce the disputed
intermediate states before their fixes. The final context cleanup subsequently
reconciled the surviving ownership and lifecycle conclusions into the current
architecture and invariant documents linked above.
