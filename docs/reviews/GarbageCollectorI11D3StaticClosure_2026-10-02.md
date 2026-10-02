# Garbage Collector I11D.3 Static Closure Audit — 2026-10-02

Gate G2 baseline: `585cfec3`, certified on 2026-09-11.

D.2h baseline: `d2d27211`, after the regional-value and persistent-edge
closure on 2026-10-01.

Audited implementation boundary: `c01e1437`, after I11D.2 dynamic-tool and
persistent-edge cost verification.

Status: complete. I11D.3a-I11D.3d distinguish the already reviewed D.2h
migration from the seven-file I11D.2 follow-up, reconcile every authoritative
ownership ledger, and rerun the deterministic production protocol witnesses.
No production repair was required. Gate G3 remains closed pending the dated
I11D.4 certification.

## Scope and Method

This is a delta audit, not a second migration. The Gate G2-to-D.2h interval
contains 203 Rust, manifest, or script files with 82,256 insertions and 18,986
deletions. That interval includes the resumable-WHNF work and D.2h's regional
value/edge cutover. Its implementation and accepted surfaces were reviewed in:

- [`GarbageCollectorAggressiveD2h_2026-10-01.md`](GarbageCollectorAggressiveD2h_2026-10-01.md);
- [`GarbageCollectorAggressiveVerificationClosure_2026-10-01.md`](GarbageCollectorAggressiveVerificationClosure_2026-10-01.md); and
- [`GarbageCollectorGCI11R002Holistic_2026-10-01.md`](GarbageCollectorGCI11R002Holistic_2026-10-01.md).

The D.2h-to-I11D.2 interval is deliberately small: seven source/tooling files,
122 insertions, and four deletions. This audit inspected that diff directly,
reran the exact inventories instead of accepting their current fingerprints,
and paired the static result with deterministic collector/runtime fixtures.

## I11D.3a — Source Delta

| Boundary | Gate G2 through D.2h | D.2h through I11D.2 | Current disposition |
| --- | --- | --- | --- |
| Unsafe | The manifest gained one deterministic allocation-bitmap read, the access-qualified `Gc::duplicate_in` raw reconstruction, test-only typed reinterpretation/inspection, and a root projection expression refactor. | No unsafe occurrence changed. | `audit-unsafe.sh` matches the checked-in manifest. The one production addition is a private raw reconstruction after matching-mutator ownership and representation validation; I11D.2e separately proves it compiles to pointer traffic only. |
| Trace and managed graph | Resumable-WHNF checkpoints and D.2h changed trace visitors and persistent carriers substantially. | No production trace or managed-family code changed. Five new persistent-edge occurrences are test-only I11D.2e code-generation/traffic witnesses. | Raw-value, persistent-edge, durable-owner, containment, recursive-identity, active-owner, and Gate G2 composition inventories all pass with no defect or pending disposition. |
| Mutation | Persistent mutation APIs changed owner/edge inputs from copied `Gc<T>` values to borrowed edges, with explicit regional duplication only when installing an edge. | No mutation code changed. | This narrows rather than adds mutation authority. The collector remains STW, and no new barrier or mutation gateway exists. |
| Managed entry | Regional value access was propagated through evaluator, compiler, reflection, net, and fixture callers. | No production entry changed. | The exact inventory remains 534 introductions and only the two private higher-ranked collector admissions. There is no production nested introduction or pending exception. |
| Locks and waits | WHNF/coordinator scheduling changed extensively and was reviewed by the completed resumable-WHNF and D.2h reviews. | No production lock, wait, coordinator, or scheduler source changed. | Coordinator registry/generation, WHNF checkpoint, evaluator-access, and resolved-call inventories pass. Forced worker/collector ordering remains latched rather than repetition-based. |
| Finalization | I11D.0 added deterministic observation and exact slot accounting; D.2h migrated fixture ownership without changing passive-drop authority. | Collector code changed only to shorten one Miri scale fixture. A runtime fixture removed a racy final instantaneous `readiness()` assertion. | Exact coordinator inventory, terminal task observation, allocation accounting, and finalizer activity remain authoritative. Focused finalizer, coalescing, retry, and terminal-teardown fixtures pass. |
| External owners | D.2h removed ambient value traits and retained the existing passive-managed versus active-external lifecycle split. | No production external-owner code changed. | Active-RAII, containment, opaque lifecycle, and runtime-retirement inventories/fixtures remain closed. |

The post-D.2h files are therefore all verification support:

- a release code-generation example and check script;
- registration of that script in the collector check;
- a smaller Miri-only width for an otherwise unchanged marking test;
- the zero-allocation/root-traffic persistent-edge latch;
- the readiness-observation fixture correction; and
- the exact five-occurrence test partition/fingerprint update.

## I11D.3b — Authoritative Ledger Reconciliation

`cargo test -q inventory` passes all 121 selected library inventory tests and
the eight selected integration tests. The primary quantitative ledgers are:

| Ledger | Current accepted surface | Closure result |
| --- | --- | --- |
| Raw `core::Value` APIs | 518 total: 485 regional access, 28 collector primitives, five regional representations | zero violation and zero pending remediation |
| Persistent managed edges | 878 total: 148 production typed, 36 production erased, 680 test typed, 14 test erased | zero defect and zero pending classification; the five-item post-D.2h delta is test-only I11D.2e evidence |
| Runtime-root publication | 272 exact occurrences | zero defect and zero nested active-access construction |
| Mutator introduction | 534 exact occurrences; 533 access-gateway and 14 construction-gateway uses, with overlapping gateway accounting | two direct higher-ranked admissions only; zero production nested introduction or pending disposition |

The durable-owner, containment/capture, active external-owner, machine-state,
WHNF checkpoint, recursive-identity, runtime-cache, compiler-boundary,
resolved-call, and Gate G2 composition inventories also pass. Inspection of
the seven changed files found no occurrence outside those accepted ledgers and
no fingerprint was updated merely to silence an unclassified change.

## I11D.3c — Protocol Boundary Audit

The current source and forced fixtures agree on these boundaries:

- **Locks and waits:** synchronous collection observes an already reserved
  target and blocked outer mutator before the fixture releases the worker.
  Collection waits outside the coordinator mutex, and fresh entrants retain
  their documented admission behavior.
- **Passive finalization:** managed destruction receives no runtime callback or
  active external-owner authority. Exact allocation accounting and coordinator,
  diagnostic, event, observation, and terminal-task state prove it publishes
  no replacement managed or scheduler work.
- **Active external ownership:** managed opaque shells contain only passive
  handles. External owner retirement and payload destruction remain explicit,
  unlocked operations, and runtime retirement makes escaped public values
  inert instead of reviving the value domain.
- **Panic and retry:** recoverable pre-publication/ordinary finalizer panics
  relatch collection and retire the collector mutator; an irreversible
  topology panic wakes waiters into durable poison rather than reopening an
  uncertain heap.
- **Terminal teardown:** teardown waits for active owner regions, provides no
  mutator to final destruction, and visits pending attached/detached topology
  under the collector's existing terminal rules.

The removed final `RuntimeReadiness::Ready` assertion in
`passive_finalization_produces_no_runtime_work` is an accepted fixture repair,
not weakened semantics. `readiness()` is an instantaneous observation and may
race a worker parking after terminal publication. The fixture's exact fixed
scheduler inventory followed by terminal task observation establishes the
no-new-work claim without freezing that incidental scheduling state.

## I11D.3d — Focused Closure

Fresh evidence at the audited boundary:

```text
crates/glam-gc/scripts/audit-unsafe.sh
    checked manifest passed

cargo test -q inventory
    121 library inventory tests passed
    8 selected integration tests passed

cargo test -q --lib api::tests::managed_collection_tests::
    9 focused production collection tests passed

focused glam-gc protocol matrix
    9 forced wait, finalizer, panic/retry, passive-root, active-owner,
    and terminal-teardown tests passed
```

The focused collector matrix includes
`synchronous_collection_waits_without_blocking_a_fresh_entrant`,
`failed_mark_attempt_does_not_publish_or_replace_a_report`,
`request_during_finalization_is_coalesced_without_blocking_admission`,
`panicking_elected_collection_restores_and_relatches_its_request`,
`panicking_finalizer_retires_its_mutator_and_relatches_collection`,
`last_public_drop_after_upgrade_is_passive_and_conservatively_retained`,
`terminal_heap_teardown_waits_for_active_owner_regions`,
`ordinary_finalization_has_a_mutator_but_terminal_teardown_does_not`, and
`irreversible_topology_panic_wakes_waiters_into_permanent_poison`.

Because I11D.2 and this audit changed no production runtime/collector code,
unsafe/tracing/mutation/root/admission/finalization behavior, or scheduler
semantics, I11D.4 may reuse the complete ordinary/aggressive workspace closure
recorded by D.2h under its explicit change-trigger policy. Routine checks and
the final dated certification remain I11D.4 work.

## Decision

I11D.3 is complete with no open finding. The static graph, ownership,
admission, mutation, finalization, and coordination boundaries agree with the
current exact ledgers and deterministic fixtures. This audit does not itself
pass Gate G3 or enable collection in ordinary production execution; I11D.4
must still publish the combined certification. Production remains immutable
`CollectionPolicy::NoAuto`.
