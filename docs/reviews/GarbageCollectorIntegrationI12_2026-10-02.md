# Garbage Collector Integration Post-I12 Review — 2026-10-02

Related plan:
[`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md),
Phase I12.

Status: **complete; I12 passes.** I12A established authoritative runtime GC
activity, explicit service, readiness, recovery, and reporting. I12B retains
immutable `CollectionPolicy::NoAuto` heaps and makes real collector pressure
actionable only at the stable runtime-pump boundary. No unresolved finding
blocks I13.

## Reviewed Scope

The review covered:

- the production heap-constructor and mutator-entry inventories;
- `RuntimeMutationAdmission` activity, request, completion, and settlement
  publication;
- direct and revision-checked collection service;
- `pump_until_stable`, readiness, settlement, and both executable batch loops;
- pressure latching, request coalescing, retry and poison precedence;
- retained-value and assembly-output survival plus unreachable reclamation;
- current architecture, roadmap, ownership-ledger, and I13 forward-plan text.

The review treats collector statistics as advisory input, not readiness
authority. `RuntimeActivityState` and its maintenance revision remain the one
authoritative runtime projection.

## Implemented Boundary

Every Glam runtime value domain still constructs one immutable `NoAuto` heap.
Ordinary and recursive mutator entry cannot collect and performs no runtime GC
publication. Explicit request/service and verification-only aggressive entry
remain the only other classified paths.

After the runtime pump has drained useful background work, abandoned
unclaimed sparks, and acquired exclusive settlement admission, it verifies
that no GC lease, background work, spark, or delivery is active. At that
stable instant it samples the collector's request latch. Pending pressure is
promoted through `RuntimeMutationAdmission` into the existing explicit-request
record and advances the maintenance revision exactly once. The pump releases
admission, issues the ordinary activity wake, and returns without collecting.

The next stable `readiness` call reports `MaintenanceRequired`. Its retained
snapshot performs the existing revision-checked service. Direct clients may
still request or service maintenance explicitly. Pure Glam computation cannot
observe policy, pressure, revisions, collection counts, or reports.

## Concurrency and Locking Audit

Pressure sampling occurs while exclusive runtime settlement admission is held.
An actual collection cannot race that sample: all collection paths first
publish an active runtime GC lease under shared mutation admission, and the
stable-pump predicate requires zero active leases. Ordinary `NoAuto`
allocation may race the sample because it cannot collect. That race is safe:
pressure arriving after the sampled heap snapshot remains in the collector
latch and is promoted by the next pump.

Completion publication uses the collector snapshot captured after its attempt.
A later allocation can raise pressure after that capture but before the older
runtime outcome publishes. The older outcome may clear the explicit request it
serviced, but cannot clear the collector's later request latch. A deterministic
pause at exactly that boundary proves the next pump retains and promotes the
later pressure. This satisfies the conservative rule that a race may cause one
redundant later attempt but may not lose a request.

The pump never holds collector state across a wake or callback. It releases
exclusive settlement admission before notifying waiters. Collection and
finalization still run outside runtime locks under the already-reviewed I12A
activity lease.

## Acceptance-Criteria Accounting

- **Construction closure:** `CoreValueFactory::new` remains the only
  production heap-policy selection and chooses `NoAuto`. Higher constructors
  delegate or retain a supplied runtime.
- **Entry closure:** only explicit service and aggressive verification may
  collect. One source-backed latch identifies the stable pump as the only
  pressure-promotion caller.
- **Observational readiness:** heap pressure alone does not alter readiness;
  authoritative promotion advances the runtime maintenance revision.
- **Coalescing:** repeated pumps, explicit-request-before-promotion, and
  promotion-before-explicit-request retain one actionable revision.
- **Forced ordering:** channel/probe fixtures force pressure before and after a
  stable snapshot, pressure after collection snapshot but before outcome
  publication, and a pump parked behind active runtime activity.
- **Client closure:** direct service, revision-checked snapshot service, the
  configured-logger batch loop, and the default-logger batch loop all service
  the same maintenance protocol.
- **Production reclamation:** real pressure followed by stable promotion and
  snapshot service reclaims the unreachable pressure fixture while a retained
  compiled assembly value remains evaluable.
- **Policy closure:** service consumes the collector request and leaves the
  heap policy `NoAuto`.

## Findings and Dispositions

### I12R-001 — Completion/pressure publication needed an exact race fixture

**Disposition: resolved.** Repetition could not prove the required ordering.
The test-only outcome-publication probe pauses after the collector has captured
its completed heap snapshot and before the runtime lease publishes it. The
fixture then raises fresh pressure and proves the old outcome cannot erase the
later collector latch.

### I12R-002 — Current documentation still described promotion as future work

**Disposition: resolved.** The evaluation architecture, source map,
integration plan, and roadmap now describe the implemented pump/readiness/
service sequence. Target-design documents remain free of bootstrap policy.

### I12R-003 — Future integration phases remain coherent

**Disposition: confirmed.** I13 remains a cleanup and documentation phase; it
does not need to repair I12 activity or pressure semantics. The separate
concurrent-collector plan may later replace the stop-the-world/manual policy,
but the current explicit boundary neither assumes automatic outer-entry
election nor blocks that redesign. Gate G4 can use the completed I12
pressure/reclamation matrix as planned.

## Verification Record

Fresh checks passed on 2026-10-02:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
    root library: 1,911 passed, 2 intentional stress ignores
    all auxiliary targets passed
cargo test -q -p glam-gc
    201 passed, 2 intentional ignores
scripts/check-interaction-net-profiling.sh
    all 21 named profiling regressions passed
```

Focused I12B tests additionally passed for stable/no-pressure pumping,
promotion/coalescing, every forced race, parked activity, production
reclamation, retained assembly output, both batch loops, constructor policy,
entry classification, and single-boundary source closure.

The release direct-assembly Hello World sample remains 166 bytes with SHA-256
`e18df1ef13e32df92ca590919decaf97ed1664c60df7747dd49c1e71577b1e01`.
This is output-equivalence evidence, not a performance threshold.

## Decision

I12 is complete. Production runtimes remain explicit `NoAuto` heaps; stable
pumping makes collector pressure actionable without adding hot-path mutator
coordination or collecting inside the pump. Proceed to I13.
