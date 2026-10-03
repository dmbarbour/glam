# Aggressive GC Verification Regression Remediation Plan — 2026-10-03

Status: open. Discovered 2026-10-03 while executing the pre-performance review
P0 item "run one aggressive workspace pass and record it"
([holistic review](../reviews/HolisticArchitecturePrePerformance_2026-10-03.md)
X2; [parallel review](../reviews/ArchitectureAndVerification_2026-10-03.md)
AR-004). The aggressive-GC verification mode is broadly red and partially
hanging on `main`, so it currently provides no coverage.

**Progress 2026-10-03:** workstream 1 (the harness panic) is fixed and
committed (`5b8f54e4`), clearing 8 of the failures. Triage then showed the
remaining 16 are two further, distinct defects — **not** the same root cause as
the panic — including a genuine pre-existing settlement/kill regression (see
the revised inventory and workstreams below).

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

**Triage after the workstream-1 fix (2026-10-03).** Re-running all 24 under
aggressive with per-test timeouts: 8 pass, 8 fail, 8 hang. This resolves the
"do the hangs share the cause?" question — they do **not**. Three distinct
defects:

- **Defect 1 — harness panic (classes A, C, D; 8 tests). FIXED** by `5b8f54e4`.
- **Defect 2 — new pressure/policy tests (class B; 4 tests).** `managed_collection_tests`/
  `runtime_tests` cases *added by I12A* (`46dc1487`), never validated under
  aggressive; they fail (exit 101) on assertions, not panics. Likely encode
  expectations that the forced-collection hook violates; needs triage to
  separate test-expectation drift from a real pressure/collection defect.
- **Defect 3 — settlement/kill regression (classes E + F; 12 tests).** These
  settlement/kill tests are **pre-existing** (present and green under aggressive
  at `d2d27211`) and `46dc1487` did **not** touch `evaluation/tests.rs` — so
  I12A's runtime changes genuinely regressed the settlement/kill path under
  forced collection. It manifests two ways: wrong result (class E,
  e.g. `forced.settle()` no longer settles an unchanged deadlock —
  `tests.rs:7871`) and **deadlock/hang** (class F,
  `forced_kill_*`/`*_settlement_*` never return). This is the serious cluster:
  a real correctness + liveness regression, not test drift.

## Fix semantics — DECIDED 2026-10-03 (Option 1, no GC)

Decision (David): go with **Option 1, no collection**. This lifecycle is niche
and inaccessible from the public API, so it does not warrant a bespoke
correctness semantics. When the admission authority is absent, run the
operation directly with no lease and no collection. Implemented in `5b8f54e4`.

When the admission authority is absent (runtime dropped, value domain still
live), what should a test/aggressive collecting entry do? Options considered:

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

1. **Fix the collecting-entry authority handling. DONE (`5b8f54e4`).** Audited
   for sibling wrappers: `with_maybe_collecting_entry` is the only panic site;
   the other `gc_activity_admission` upgrades in `core.rs` use `if let Some`.
2. **Defect 2 — triage the new pressure/policy tests (class B, 4).** Determine,
   per test, whether the failure is an expectation that the forced-collection
   hook legitimately violates (fix the test) or a real pressure/collection
   accounting defect in I12A (fix the runtime). These are new tests, so a
   test-side fix is plausible, but confirm against the pressure/maintenance
   contract rather than assuming.
3. **Defect 3 — the settlement/kill regression (classes E + F, 12).** The
   serious one. Investigate why forced collection at outer entry breaks the
   coordinator settlement/kill path — wrong `settle()` result (E) and deadlock
   (F). Likely a root reclaimed mid-settlement or a lock/wait that forced
   collection disrupts. May warrant its own focused debugging; capture the
   lock-order / root-retention invariant it violates. Fix in the runtime, not
   the (pre-existing, previously-green) tests.
4. **Add targeted regressions.** Done for defect 1 lifecycle (value domain
   outlives runtime, then a managed entry under aggressive — pending as a named
   test). Add boundary regressions for defects 2/3 per AR-005: any change to
   root publication or collecting-entry placement must force
   collection/suspension across that exact boundary.
5. **Record a fresh green aggressive baseline.** Revision, `rustc --version`,
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
