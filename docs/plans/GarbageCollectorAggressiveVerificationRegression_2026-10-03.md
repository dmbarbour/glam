# Aggressive GC Verification Regression Remediation Plan — 2026-10-03

Status: complete on 2026-10-04. A green aggressive baseline is recorded below.
Discovered 2026-10-03 while executing the pre-performance review P0 item "run
one aggressive workspace pass and record it"
([holistic review](../reviews/HolisticArchitecturePrePerformance_2026-10-03.md)
X2; [parallel review](../reviews/ArchitectureAndVerification_2026-10-03.md)
AR-004).

## Summary

The repository's `aggressive-gc-verification` mode was red and hanging on
`main`, so it provided no coverage. Bisection identified I12A explicit GC
maintenance (`46dc1487`) as the first bad commit. Every failure traced to one
placement decision: the aggressive hook took a runtime GC-activity lease (the
runtime mutation-admission gate plus a maintenance-revision bump) at every
value-domain entry. The post-I12A design never treats that as a GC-safe
boundary. Production builds were unaffected; the hook was compiled only into
test and verification builds.

The resolution removes the per-entry hook entirely. Aggressive verification
now reuses the one existing NoAuto collection decision: it replaces only that
decision's pressure input and has the stable pump service the request it
promotes. Settlement validation also stops comparing the raw maintenance
revision, an independent semantic correction that the investigation exposed.

Earlier drafts of this plan attributed the settlement/kill failures to a
production regression in I12A. Later investigation corrected that; see
"Investigation record".

## Purpose of aggressive verification

The mode exercises root ownership under real collections without becoming
production policy. Heap policy remains `NoAuto` in every build.

- Before: `glam-gc` forced a full collection before each eligible outer mutator
  entry, and `glam` wrapped every value-domain entry in a runtime GC lease.
  See the completed
  [2026-09-11 remediation](GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md).
- After: the stable runtime pump treats any allocation since its previous
  evaluation as pressure and services the resulting maintenance request
  itself. Collections use the production boundary and service path.

## Discovery and reproduction

On `main` before remediation:

```sh
cargo test --workspace -q --features aggressive-gc-verification
```

The run reached about 1065 of 1911 library tests, then stalled behind
settlement/kill tests that hung for more than 60 seconds; it was killed.
Sixteen failures and eight hangs were observed, a lower bound.

`git bisect` over `d2d27211..HEAD` used the pre-existing oracle
`api::tests::public_values_retain_only_the_runtime_value_domain`. The range runs
from the last recorded green aggressive pass
([closure review](../reviews/GarbageCollectorAggressiveVerificationClosure_2026-10-01.md))
to the head. The oracle script pinned `+1.99.0`, skipped commits that did not
build or lacked the oracle, and treated a timeout as bad. First bad commit:

- **`46dc1487` "Complete I12A explicit GC maintenance"**. Its parent `6aaaaeec`
  is good. Reviewed in
  [`GarbageCollectorExplicitMaintenance_2026-10-02.md`](../reviews/GarbageCollectorExplicitMaintenance_2026-10-02.md).

## Investigation record

Each finding below was confirmed by reproduction or by reading the code path.

1. **Panic when a value domain outlives its runtime.** I12A's test/aggressive
   wrapper `CoreValueFactory::with_maybe_collecting_entry` upgraded a weak
   `gc_activity_admission` with `.expect(...)`. A `Values` handle legitimately
   retains the value domain after its runtime and admission authority drop;
   `public_values_retain_only_the_runtime_value_domain` asserts exactly that.
   The next managed entry panicked. This explained the diagnostics,
   value-domain-retention, and WHNF-checkpoint failure classes.
2. **Triage after fixing (1): 8 pass, 8 fail, 8 hang** among the 24 observed
   failures. The rest had other mechanisms.
3. **Settlement revision churn.** `settle()` returned
   `RuntimeSettlementError::RuntimeChanged`. Settlement compared the probe's
   GC maintenance revision with the current revision. Every lease
   `begin_gc_activity` and `finish_gc_activity` advances that revision, even
   for a `no_collection` lease. The aggressive hook leased every entry, so the
   revision moved between any probe and its commit.
4. **New I12A pressure tests.** These asserted that ordinary entries neither
   advance the revision nor service pressure. The aggressive hook violated
   that by construction.
5. **Settlement commit self-deadlock.** After decision D2 let validation pass,
   the remaining hangs became visible. The runtime gate is a non-reentrant
   `RwLock`. Settlement holds `gate.write()` while it constructs managed
   values, including `runtime_killed_failure` and report roots. Under the hook,
   each construction called `begin_gc_activity`, which takes `gate.read()` on
   the same thread. That self-deadlock produced the kill/settlement hangs.
   Production construction never touches the gate. The gate was designed as a
   publication boundary, and the hook made allocation reach it.
6. **`glam-gc` was already correct.** Its pre-entry collection fires only for an
   outer entry with no active mutator on the thread (`heap.rs`
   `with_mutator_after_admission`). Its idle election also requires zero active
   outer mutators. The defective boundary was entirely in `glam`'s lease
   wrapper.
7. **The existing manual decision.** In `glam` there is exactly one
   pressure-based "should we collect" decision:
   `RuntimeMutationAdmission::promote_gc_pressure_request`, called only by
   `pump_until_stable`. It runs at a stable boundary while exclusive settlement
   admission is held. It only publishes a request; collection happens through
   explicit service after the settlement guard is released, outside every
   mutator. `ENTRY_INVENTORY` agreed: the aggressive hook was the sole
   `MayElect` entry.

## Decisions

Each decision states its reasoning and the alternatives that were rejected.

### D1 — A value domain without a runtime does not collect (superseded by D5)

When the admission authority is absent, run the operation with no lease and no
collection. This lifecycle is niche and unreachable from the public API, so it
does not warrant a bespoke correctness semantics. Keeping the authority alive
as long as the value domain was rejected. It would retain the scheduler and
profile, violating the no-backedge invariants the retention tests prove.

Implemented in `5b8f54e4`. D5 then deleted the wrapper. The decision still
holds, because no value-domain entry collects in any build.

### D2 — Settlement validation does not compare the maintenance revision

`validate_quiescence_guarded` no longer rejects on
`maintenance.revision != snapshot.stamp.gc_maintenance_revision`.

Reasoning:

- The collector is non-moving and preserves every registered root. A collection
  between probe and commit therefore cannot change what settlement depends on.
  Work generation, exits, observations, and retained roots are identical before
  and after.
- Validation already observes that instant directly: work generation and exits,
  observation epoch, empty outputs, and a clean maintenance state (no active
  lease, no request, `Idle` disposition).
- A failed collection surfaces through `disposition` or `explicit_request`. An
  in-flight collection surfaces through `active_leases`.
- The raw revision was a conservative proxy. It conflated "a collection ran"
  with "the instant changed".
- The revision remains the compare-and-swap token for the explicit-service path
  (`begin_gc_activity_for_snapshot`), where exactly one of several racing
  services must win. Settlement is serialized by the exclusive guard and does
  not need it.

Rejected alternative: narrow `advance_revision` so that no-op leases do not
bump it. That would keep the over-conservative contract, and its effectiveness
depended on how often collections happened to run.

Tests adapt to the intended semantics, not the reverse.
`production_collection_preserves_each_serial_boundary` previously asserted the
opposite, introduced by I12A. It now asserts that a pre-collection snapshot
still validates and settles. A settled report records the maintenance revision
current at commit, so the test compares the report's work generation and
observation epoch with the snapshot. It also asserts that the report's
revision is later than the snapshot's.

### D3 — Settlement is recorded as a known concurrent-GC hazard

D2 and finding (5) both rest on premises that hold only while collection is
confined to explicit stable-boundary service. They are recorded as Open Design
Gate 9 of the [concurrent collection plan](ConcurrentGarbageCollection_2026-08-28.md)
for deliberate review when that work begins. The settlement concept needs deep,
critical review at that time.

### D4 — Rejected fixes for the settlement deadlock

All four addressed symptoms of a hook at the wrong boundary rather than
removing it.

- **Skip the lease when `gate.try_read()` fails during settlement.** This
  bypasses the runtime's GC control to dodge an observed conflict.
- **Make the settlement gate publication-only** by constructing values before
  taking it. This is larger, and it restructures production settlement to
  accommodate a verification hook.
- **Drive the runtime lease from `glam-gc`'s outer-admission boundary.** This
  fixes the churn but not the deadlock, because the commit's construction is
  itself an outer entry with no active mutator.
- **Register settlement as an active region** so that its allocations look
  nested. This adds a new cross-crate mechanism solely for a verification
  trigger.

### D5 — Aggressive mode reuses the existing manual decision

Under the feature, only the pressure input of `promote_gc_pressure_request`
changes. Every other promotion condition is unchanged: a stable boundary, no
active lease, no pending request, and an `Idle` disposition.

Reasoning: NoAuto already places its one collection decision at a correct
boundary. Verification collections now follow the production path:

1. pump to stability;
2. promote under exclusive settlement admission;
3. release the guard;
4. explicitly service outside every mutator, with a proper lease.

Correctness is inherited rather than re-derived, and no allocation path takes
the runtime gate. That removes findings (1), (3)'s per-entry churn, (4)'s
per-entry leasing, and (5) together.

### D6 — Pressure means new allocations, not unconditional `true`

With a literal `true`, every client loop shaped like the CLI's
`settle_batch_runtime` would livelock. That loop runs
`pump → MaintenanceRequired → service → continue`, and each pump after a service
would promote again. Pressure must converge, so it is "any allocation since the
previous evaluation".

- **Counter.** The pressure count is `HeapMetrics::class_cache_hits() +
  class_cache_misses()`. Every production allocation (`Allocator::alloc` →
  `ThreadCacheHandle::try_allocate`) records exactly one hit or one miss. The
  thread caches publish these counts when the outer region closes, so they are
  current at a stable boundary. Collection and passive finalization do not
  allocate.
- **Safety.** `Heap::metrics()` panics after poison and scans allocation words.
  It is called only after the maintenance snapshot shows a usable heap. The
  scan's cost is acceptable in verification builds.
- **Record even on refusal.** The count is recorded even when promotion is then
  refused. At a stable boundary, refusal means a pending or retry-required
  collection already covers those allocations. Exclusive settlement admission
  serializes evaluation, so the `AtomicU64` swap cannot race.

### D7 — Under the feature, the pump services its own promotion

After promoting, `pump_until_stable` calls `service_managed_collection` and
continues toward stability.

Reasoning:

- Readiness observed after a pump then matches ordinary mode. 24 of 55 test
  `pump_until_stable()` sites immediately match `Ready` or `Deadlocked`.
- This is the safest point: stable, outside every mutator, and after the
  settlement guard is dropped.
- A failed service is retained by maintenance state and reported by readiness.
  It blocks re-promotion, so the loop converges.
- The departure from "the pump never collects" applies only under the feature.
  It is documented in the pump's rustdoc and in
  [`architecture/evaluation.md`](../architecture/evaluation.md).

### D8 — The per-entry machinery is erased from `glam`

Removed:

- `with_maybe_collecting_entry`; both entry points now call `heap.with_mutator`
  directly;
- `gc_activity_for_entries`;
- `gc_activity_admission` and `attach_gc_activity_admission`;
- `enable_collection_before_outer_entry_for_test` and `_for_verification`;
- the constructor call that enabled the hook.

`glam-gc`'s `collect_before_outer_entry` stays. It is outside this
`glam`-only remediation and `glam-gc`'s own tests still use it. It is now
unused by `glam` and a candidate for separate `glam-gc` cleanup.

### D9 — The coverage tradeoff is accepted

Collections now happen roughly once per stable settlement cycle instead of
before every outer entry. Aggressive mode no longer forces a deterministic
failure at each allocation/publication boundary inside one evaluation. Tests
that care about a specific boundary request a collection explicitly, which also
names the boundary in the test.

### D10 — Test dispositions for the hook-dependent tests

Each was judged on purpose.

- **Retired:** `aggressive_debug_collection_runs_before_outer_runtime_entry`. It
  tested the deleted mechanism itself. Its still-valid assertion, that heap
  policy stays `NoAuto`, moved into the rewritten test below.
- **Rewritten:** `repository_aggressive_mode_enables_each_production_runtime`
  became `repository_aggressive_mode_services_new_allocations_at_each_stable_pump`.
  It verifies the new mechanism: no collection at value-domain entries,
  exactly one service per stable pump after new allocations, no repeat
  without allocations, `Ready` readiness, and immutable policy.
- **Un-gated:**
  `collection_between_polls_preserves_only_the_installed_checkpoint` and
  `blocked_client_checkpoint_survives_collection_until_promise_assignment`.
  Both already collect explicitly at the boundary they verify. The hook only
  added extra collections. They now also run in every ordinary `cargo test`.
- **Inventory:** `gc_activity_inventory.rs` replaces the `MayElect` record
  with an explicit-service record. It asserts that no entry may elect
  collection, and it keeps two durable negative rules: value-domain entries
  never take a GC lease, and the value domain holds no GC activity authority.

### D11 — Tests primarily about NoAuto are disabled under the feature

The criterion, set by the maintainer, is that a test primarily testing `NoAuto`
behavior does not run under `aggressive-gc-verification`. Under the feature,
the pump replaces the pressure input and completes the request, so these
properties cannot hold. Each of the seven tests that failed was judged
individually.

Five tests primarily test the production pressure-promotion protocol:

1. collector pressure latch;
2. stable-pump promotion;
3. client-visible `MaintenanceRequired`;
4. client service, including coalescing and wake timing.

These five carry `cfg(not(feature = "aggressive-gc-verification"))`:

- `stable_pump_without_pressure_changes_no_maintenance_state`
- `stable_pump_promotes_pressure_once_without_changing_policy`
- `pressure_after_one_stable_snapshot_waits_for_the_next_pump`
- `explicit_request_and_pressure_promotion_coalesce_in_both_orders`
- `promoted_pressure_reclaims_dead_allocations_and_preserves_assembly_result`

They continue to run in ordinary builds, where the protocol applies.

Two tests are primarily about something else, and only their final readiness
expectation is NoAuto protocol. They still run under the feature, with only
that tail gated:

- `pressure_after_collection_snapshot_survives_older_outcome_publication`
  tests a maintenance-state race: publishing an older outcome must not clear
  later collector pressure. The race holds in both modes. The final "pump, then
  `MaintenanceRequired`" step is gated.
- `parked_pump_promotes_pressure_after_activity_wake` tests pump liveness. The
  pump parks behind an active lease, pressure alone cannot bypass it, and lease
  retirement wakes it. Under the feature, the final assertion expects `Ready`,
  because the woken pump services its own promotion (D7).

These two were first gated whole. Strict all-features Clippy then flagged the
outcome-publication probe as dead code, and re-judging the tests showed that
whole-test gating would have discarded their race and liveness coverage.

## Verification record

Environment: `rustc 1.99.0 (b940084d7 2026-09-28)`, Linux 6.8.0 x86-64,
8 hardware threads. Revision: base `f5702dcd` plus this remediation, recorded
at the commit that introduces this record.

| Check | Result |
| --- | --- |
| `cargo fmt --check`; strict Clippy, workspace, all targets, with and without all features | Clean. |
| Ordinary `cargo test -p glam --lib` | 1907 passed, 0 failed, 2 ignored. Net +1 over the pre-remediation 1906: one retired test and two tests un-gated from aggressive-only. |
| `cargo test --workspace --features aggressive-gc-verification` before D11 gating | Library: 1901 passed, 7 failed (exactly D11's seven), no hangs. |
| Same command after D11 gating: **green aggressive baseline** (run inside `check.sh full`) | No hangs. `glam` library 1903 passed, 2 ignored (182 s); binary 81; integration `cli` 49, `effect_embedding` 1, `executable_samples` 5, `hello_assemblies` 1, `invalid_samples` 2, `macro_protocols` 5, `public_api` 46, `sample_sources` 5, `source_doc_coupling` 1; `glam-gc` 214 passed, 2 ignored; Loom 7; doctests 2 and 8. |
| `scripts/check.sh full` | Passed in 911 s. Ordinary workspace: `glam` library 1907 passed. `glam-gc` check suite, G0 semantics, and all 21 profiling fixtures passed. Cursor stress passed 2 and million-edge scale passed 2. Miri and sanitizers were skipped because no nightly toolchain is installed. |

`dictionary_tag_and_tuple_patterns_match_or_fall_through` reports "running for
over 60 seconds" in both modes. That is its known debug-build cost, not a hang.

## Follow-ups

- Remove `collect_before_outer_entry` from `glam-gc` if no `glam-gc` test needs
  it independently.
- Keep D3's settlement review attached to concurrent collection.

## Cross-references

- Pre-performance reviews:
  [holistic X2](../reviews/HolisticArchitecturePrePerformance_2026-10-03.md),
  [parallel AR-004](../reviews/ArchitectureAndVerification_2026-10-03.md).
- Culprit `46dc1487` and its review
  [`GarbageCollectorExplicitMaintenance_2026-10-02.md`](../reviews/GarbageCollectorExplicitMaintenance_2026-10-02.md).
- Prior remediation
  [`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`](GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md)
  and closure
  [`GarbageCollectorAggressiveVerificationClosure_2026-10-01.md`](../reviews/GarbageCollectorAggressiveVerificationClosure_2026-10-01.md).
- Settlement hazard: Open Design Gate 9 of
  [`ConcurrentGarbageCollection_2026-08-28.md`](ConcurrentGarbageCollection_2026-08-28.md).
