# Aggressive GC Verification Regression Remediation Plan — 2026-10-03

Status: open. Discovered 2026-10-03 while executing the pre-performance review
P0 item "run one aggressive workspace pass and record it"
([holistic review](../reviews/HolisticArchitecturePrePerformance_2026-10-03.md)
X2; [parallel review](../reviews/ArchitectureAndVerification_2026-10-03.md)
AR-004). The aggressive-GC verification mode is broadly red and partially
hanging on `main`, so it currently provides no coverage. Root cause is
identified and confirmed; the fix semantics need a decision (see below) before
implementation.

## Purpose

`aggressive-gc-verification` forces a collection before each eligible outer
entry into a runtime value domain, turning a regional ownership gap into a
deterministic failure at the former allocation/publication boundary. It is not
a production policy, a performance mode, or a reason to weaken semantic tests.
See the completed
[Aggressive GC Verification Remediation Plan — 2026-09-11](GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md)
for the mode's intent and the I11D.1 remediation that last left it green.

This plan addresses a *new* regression: the verification harness itself now
panics on a basic, supported lifecycle, so it cannot run to completion.

## Discovery and reproduction

Documented command on `main`:

```sh
cargo test --workspace -q --features aggressive-gc-verification
```

The run was RED and HANGING. It reached ~1065/1911 before settlement/kill tests
hung (>60s) and progress stalled; it was killed. **16 failures and 8 hangs were
observed — a lower bound, since ~45% of the suite never executed.**

`git bisect` over `d2d27211..HEAD` (the window since the last recorded green
aggressive pass, `GarbageCollectorAggressiveVerificationClosure_2026-10-01.md`)
using the pre-existing oracle
`api::tests::public_values_retain_only_the_runtime_value_domain`
(green at `d2d27211`) identified the first bad commit:

- **`46dc1487` "Complete I12A explicit GC maintenance"** (parent `6aaaaeec`
  "Complete I12A.0 GC readiness review" is good). A large commit (+2131/−92,
  23 files; `src/runtime.rs` +497, `src/core/managed.rs` +200,
  `crates/glam-gc/src/heap.rs` +119, new `src/api/runtime/readiness.rs`),
  reviewed in
  [`GarbageCollectorExplicitMaintenance_2026-10-02.md`](../reviews/GarbageCollectorExplicitMaintenance_2026-10-02.md).

Because the oracle is pre-existing and was green before this commit, this is a
genuine regression, not merely new tests that were never validated.

## Root cause (confirmed)

The oracle fails with a **panic in product code**, not a test assertion:

```
panicked at src/core/managed.rs:510:14:
a potentially collecting runtime entry must retain its activity authority
```

I12A added `CoreValueFactory::with_maybe_collecting_entry`
(`src/core/managed.rs:496`), which wraps managed-allocation outer entries under
test/aggressive builds:

```rust
#[cfg(any(test, feature = "aggressive-gc-verification"))]
fn with_maybe_collecting_entry<R>(&self, operation: impl FnOnce() -> R) -> R {
    if !self.domain.gc_activity_for_entries.load(Acquire) { return operation(); }
    let admission = self.domain.gc_activity_admission
        .lock().expect("runtime GC activity binding was poisoned")
        .upgrade()
        .expect("a potentially collecting runtime entry must retain its activity authority");
    let lease = admission.begin_gc_activity();
    let result = operation();
    lease.finish(RuntimeGcLeaseOutcome::no_collection(self.domain.heap.maintenance_snapshot()));
    result
}
```

`gc_activity_admission` is a `Mutex<Weak<RuntimeMutationAdmission>>` stored on
the **value domain** (`src/core.rs:333`), attached from the runtime during
construction (`src/api/runtime.rs:601`). The `RuntimeMutationAdmission`
authority is owned by the runtime.

The value domain, however, is designed to **outlive its runtime**: a public
`Values` / `CoreValueFactory` handle retains the value domain after the runtime
(scheduler, coordinator, profile, admission authority) is dropped. This is a
supported, tested invariant — it is precisely what the oracle asserts
(`value_domain.upgrade().is_some()` after `drop(runtime)`). When such a handle
performs any managed entry under test/aggressive mode after its runtime is
gone, `gc_activity_admission.upgrade()` returns `None` and the `.expect(...)`
panics.

**Severity characterization:**

- **Production: unaffected.** `with_maybe_collecting_entry` is
  `#[cfg(any(test, feature = "aggressive-gc-verification"))]`; the production
  `cfg(not(...))` arm simply runs the operation. No production path upgrades
  this weak.
- **Verification: high.** The aggressive safety net panics on the most basic
  "values outlive runtime" lifecycle, so it runs to completion for essentially
  no post-runtime-drop test. It currently provides **zero** aggressive
  coverage and masks any real root-retention regressions across the subsystem,
  including the ~45% of the suite that never ran.

## Symptom inventory (lower bound)

All six classes are consistent with a single shared instrumentation panic
triggered whenever a test drops/retires a runtime or owner and then performs a
managed entry:

- A. Diagnostics retention — `api::tests::diagnostic_tests` (4)
- B. Managed-collection policy/pressure — `api::tests::managed_collection_tests`,
  `api::tests::runtime_tests` (4)
- C. Value-domain retention — `api::tests` (3, incl. the oracle) — **confirmed
  = managed.rs:510 panic**
- D. WHNF checkpoint across collection — `eval::whnf::w1c_tests` (1)
- E. Settlement/deadlock report roots — `evaluation::tests` (4, failing)
- F. Settlement/kill **liveness hangs** — `evaluation::tests` (8, >60s)

Class F (hangs) is **not yet confirmed** to share the root cause. A re-check of
one hang (`forced_kill_publishes_task_status_and_fails_owned_promises`) under
aggressive still hung with no panic captured on the main thread — consistent
with either (a) a worker-thread panic poisoning a lock and deadlocking waiters,
or (b) a distinct liveness defect. Resolving this is a plan step, not an
assumption.

## Decision required: fix semantics

When the admission authority is absent (runtime dropped, value domain still
live), what should a test/aggressive collecting entry do? Options:

1. **Run without a lease (recommended default).** If the weak upgrade fails,
   skip `begin_gc_activity`/`finish` and just run the operation. Rationale:
   with no runtime there are no workers/mutators to coordinate against; the sole
   `values` holder is the only accessor. This restores the supported lifecycle
   and keeps non-collecting correctness. Open sub-question: should it still
   force a heap-level collection directly (maximizing verification value for a
   standalone value domain), or treat "no runtime" as "no collection"?
2. **Keep the admission authority alive as long as the value domain.** Rejected
   unless shown otherwise: `RuntimeMutationAdmission` likely transitively
   retains scheduler/coordinator/profile, which would violate the "no backedge"
   invariants that `public_values_retain_only_the_runtime_value_domain` and
   `runtime_value_domain_has_no_scheduler_or_profile_backedge` exist to prove.

The `.expect("...activity authority")` encodes an assumption that is false for
the value-domain-outlives-runtime lifecycle; the fix must replace it with a
defined behavior, not merely downgrade the panic.

## Remediation workstreams

Kept separate so a harness fix cannot conceal a real product defect.

1. **Fix the collecting-entry authority handling.** Implement the chosen
   semantics in `with_maybe_collecting_entry` (and audit any sibling
   test/aggressive entry wrappers added by I12A for the same weak-upgrade
   assumption). Decide the "collect vs no-collect when runtime absent"
   sub-question.
2. **Re-run the full aggressive pass and triage the residual.** The first run
   hung at ~55%; the real blast radius is unknown. Re-run to completion and
   categorize what remains after the panic is fixed. **Explicitly confirm
   whether the class-F hangs disappear** (same root cause) or persist (a second,
   liveness defect needing its own fix — forced-collection interaction with the
   settlement/kill lock order).
3. **Add targeted regressions.** A focused test that constructs a value domain,
   drops its runtime, then performs a managed allocation entry under
   test/aggressive mode (the exact failing lifecycle), plus whatever boundary
   tests the residual triage in step 2 demands. Per AR-005, any change to root
   publication or collecting-entry placement must force collection/suspension
   across that exact boundary.
4. **Record a fresh green aggressive baseline.** Revision, `rustc --version`,
   host, wall time, and pass counts, in this plan's closure note and/or a dated
   review. Confirm `scripts/check.sh full` is green (it runs the aggressive
   pass as a gate and currently surfaces this regression).

## Verification / acceptance

- `cargo test --workspace --features aggressive-gc-verification` completes GREEN
  with no hangs.
- `scripts/check.sh full` passes.
- Regression(s) added for the reclaimed/absent-authority entry path and any
  residual class-F liveness defect.
- Production (default-feature) behavior and the "values outlive runtime,
  no scheduler/profile backedge" invariants remain unchanged.
- A dated green aggressive baseline is recorded and linked from the
  pre-performance reviews' P0 item.

## Cross-references

- Pre-performance reviews:
  [holistic X2](../reviews/HolisticArchitecturePrePerformance_2026-10-03.md),
  [parallel AR-004](../reviews/ArchitectureAndVerification_2026-10-03.md).
- Culprit commit `46dc1487` and its review
  [`GarbageCollectorExplicitMaintenance_2026-10-02.md`](../reviews/GarbageCollectorExplicitMaintenance_2026-10-02.md).
- Prior aggressive remediation
  [`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`](GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md)
  and closure
  [`GarbageCollectorAggressiveVerificationClosure_2026-10-01.md`](../reviews/GarbageCollectorAggressiveVerificationClosure_2026-10-01.md)
  (last green aggressive pass, `d2d27211`).
