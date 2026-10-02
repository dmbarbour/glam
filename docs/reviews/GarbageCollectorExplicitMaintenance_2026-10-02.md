# GC Explicit Runtime Maintenance Review — 2026-10-02

Related plan:
[`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md),
Phase I12A.

Status: **complete; I12A passes.** Immutable `NoAuto` runtimes now expose
explicit, readiness-integrated collection without adding synchronization to
ordinary production value access. Automatic collection remains disabled and
requires I12B.0's separate construction-policy decision.

## Scope and Outcome

The implementation closes the five I12A checkpoints:

- one runtime-owned maintenance record tracks active leases, an exact
  revision, explicit requests, retry/poison disposition, and durable plus
  pending failure ledgers;
- one unwind-safe activity lease brackets collection and finalization outside
  runtime admission locks;
- readiness exposes actionable maintenance, and settlement validates the
  maintenance revision alongside coordinator and observation state;
- clients can request or synchronously service a full collection without
  receiving a heap or mutator capability;
- recoverable trace/finalizer panics, successful retry, and irreversible
  poison have distinct durable outcomes; and
- batch settlement services recoverable work, renders usable-heap failures as
  structured diagnostics, and falls back to host text after poison.

The existing policy did not change. Every runtime still constructs
`CollectionPolicy::NoAuto`; ordinary pressure is advisory until an embedding
client deliberately requests or services maintenance.

## Overhead Boundary

The overhead concern is valid, but I12A keeps it outside the ordinary
production path:

- the standard `CoreValueFactory` entry facade compiles to an inline direct
  call; the activity-authority weak reference, enable flag, branch, and lease
  exist only under tests or `aggressive-gc-verification`;
- ordinary runtime mutation already acquired the activity mutex to publish its
  parking generation, so adding maintenance fields to that same protected
  record introduces no second lock acquisition;
- readiness takes one larger snapshot from that existing mutex, which is a
  cold stability operation rather than an evaluator step; and
- explicit request/service and failure reporting are intentionally cold paths.

The source-backed
`ordinary_no_auto_entries_compile_without_runtime_lease_work` latch and the
runtime revision assertion in
`explicit_managed_collection_request_is_actionable_runtime_state` prevent the
ordinary path from quietly inheriting verification-mode lease traffic.

This is not an argument that future automatic collection is free. I12B.0 must
measure and choose how automatic heaps register potentially collecting outer
entries. Moving today's test-only authority/branch into production without
that review would violate this phase's boundary.

## Concurrency and Linearization

Lease admission and retirement occur under shared `RuntimeMutationAdmission`;
readiness and settlement observe them under exclusive admission. Collection,
tracing, sweeping, and finalization run after the admission guard is released.
The existing activity wake is published after each authoritative transition.

Forced schedules establish:

- exclusive observation cannot race between lease registration and revision
  publication;
- readiness remains `Busy` until the final active lease retires;
- a finalizer-held lease prevents a pump from settling, and retirement wakes
  the parked pump;
- two threads servicing the same retained readiness snapshot admit exactly one
  collection, while the loser receives `RuntimeChanged`;
- a request before completion coalesces into that completion, while a request
  after completion remains actionable; and
- an older failed outcome cannot restore retry state after a later successful
  collection epoch.

The collector's successful epoch is copied into its non-panicking maintenance
snapshot. It is operational ordering evidence only; Glam observation epochs
and pure evaluation state remain unchanged.

## Failure and Reporting Review

A caught reversible trace panic produces `CollectorPanic`; a panic with a
durable pending finalizer suffix produces `FinalizerPanic`. Both append one
durable failure, expose `RetryRequired`, and permit explicit retry. Success
clears the outstanding work but not history. Settlement transfers only the
pending-report copy and keeps the complete failure collection authoritative.

An irreversible topology panic exercises the real collector/runtime path:
the heap publishes poison, the runtime catches the unwind, the lease retires,
and readiness becomes `MaintenanceFailed`. Later request attempts are caught
and retain terminal readiness. No default diagnostic enrichment enters that
heap; batch mode uses fallback host rendering and drops the runtime.

Maintenance failure independently makes a batch result nonzero even after a
successful retry. The default logger represents usable-heap failures as
ordinary error diagnostics with a structured `maintenance_failure` context.

## Pressure and Policy Review

The pressure fixture publishes enough large typed slots to cross the real run
high-water threshold. Repeated ordinary outer entries neither collect nor
advance the maintenance revision. Explicit service advances the collection
epoch and consumes the collector's advisory request. A companion test checks
that request plus service leaves heap policy exactly `NoAuto`.

The public maintenance report exposes scalar collection and pressure metrics,
including roots, traced/marked/reclaimed/finalized slots, reclaimed and
assigned runs, headroom, and pending finalizers. It exposes neither heap nor
mutator access and cannot be observed by Glam evaluation.

## Implementation Drift and Findings

### I12AR-001 — Ordinary-entry overhead remains structurally absent

Disposition: **verified and resolved.** The originally selected model allowed
no lease for non-collecting `NoAuto` entries. The implementation goes further
by compiling the branch and authority fields out of ordinary builds. The
test/aggressive facade pays the lease because that mode may collect before an
entry. No corrective change is required in I12A.

### I12AR-002 — Maintenance state shares the activity mutex

Disposition: **intentional and resolved.** I12A.0 described a mutex-protected
record while asking that parking generation remain logically separate. The
implementation stores both in one `RuntimeActivityData` mutex, but maintains
distinct generation and maintenance revision counters. This preserves the
validation rule and avoids another lock in every mutation wake.

### I12AR-003 — Verification fixtures expanded exact source inventories

Disposition: **reconciled.** The active-RAII inventory now classifies the
unwind-safe lease, the containment inventory classifies the panic payload
boundary, and persistent-edge/mutator inventories classify the recoverable
panic and pressure fixtures. No production managed edge or mutator gateway was
added.

### I12AR-004 — Automatic-entry cost and policy remain open

Disposition: **deferred by design to I12B.0.** I12A supplies correctness,
pressure, survivor, and boundary data, but does not justify enabling an
automatic policy or paying an entry lease on every value region. I12B.0 must
compare explicit boundary service with automatic outer-entry election and
record latency, throughput, and pause effects before selecting policy for new
runtimes.

No unresolved correctness finding blocks I12B.0.

## Forward-Plan Review

I12B.0 remains correctly sequenced after I12A. Its decision must treat the
following as current implementation facts:

- runtime policy is immutable after construction;
- explicit maintenance already gives embeddings a complete supported path;
- ordinary production entries contain no activity branch today;
- automatic or routinely concurrent entry requires production activity
  authority to be installed before the first potentially collecting access;
- poison and retry reporting must reuse I12A state rather than invent another
  scheduler channel; and
- changing policy for new runtimes must not reinterpret pressure as Glam
  semantic work.

I13's redundant-ownership cleanup is unaffected. Concurrent or moving GC,
generational policy, weak references/ephemerons, and root-frame optimization
remain in their existing deferred plans.

## Verification Record

Fresh I12A closure checks passed:

```text
cargo fmt --all --check
    passed

cargo clippy --all-targets --all-features -- -D warnings
    passed

cargo test -q
    root library: 1,900 passed, 2 intentional stress ignores
    every auxiliary target passed

cargo test -q -p glam-gc
    collector library: 201 passed, 2 scale-only ignores
    7 Loom models and 8 documentation tests passed

scripts/check-interaction-net-profiling.sh
    21 named profiling regressions passed

cargo test -q --features aggressive-gc-verification --lib \
  repository_aggressive_mode_enables_each_production_runtime
    1 passed
```

Focused forced-order tests additionally cover lease admission/retirement,
stale readiness, concurrent snapshot service, finalizer-held pump parking,
request/completion order, recoverable trace and finalizer panic, permanent
poison, pressure crossing, and immutable policy. No test relies on repetition
as evidence for a concurrent order.
