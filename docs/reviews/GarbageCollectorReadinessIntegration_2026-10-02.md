# GC Operational Activity and Readiness Review — 2026-10-02

Related plan:
[`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md),
Phase I12A.0.

Status: **complete; I12A is implementation-ready.** This review selects one
runtime-owned GC maintenance state, an admission-scoped operational lease, and
explicit `MaintenanceRequired` / `MaintenanceFailed` readiness dispositions.
It does not enable collection, change the immutable `NoAuto` production
policy, or make collector statistics authoritative.

## Current Boundary

The existing runtime already has the correct atomic publication boundary but
does not yet represent GC activity within it:

- `RuntimeMutationAdmission` gives ordinary publications shared admission and
  readiness/settlement exclusive admission;
- `RuntimeActivityState` is only a parking generation and condition variable;
  it is deliberately not a readiness stamp;
- readiness stamps contain coordinator work generation and reflection/event
  observation epoch, but no GC maintenance revision;
- every production value-domain mutator entry converges on the two private
  `CoreValueFactory` entries in `core/managed.rs`;
- the private runtime maintenance call reaches `Heap::collect_full` without
  mutation admission because I11 used only caller-established serial
  boundaries; and
- a finalizer panic terminally retires the panicking allocation, leaves the
  untouched suffix in the collector's durable finalization batch, restores
  ordinary collector admission, and resumes the Rust panic. A later full
  collection can retry that suffix.

`Heap::activity()` and `Heap::statistics()` accurately describe one collector
instant, but they cannot validate runtime readiness or prevent the classic
observe-then-sleep lost wake. They also panic after permanent heap poison. The
runtime therefore needs its own authoritative state and a non-panicking way to
project collector disposition after an attempted maintenance operation.

## Selected Authoritative Model

`RuntimeMutationAdmission` remains the only runtime-wide authority boundary.
Its sibling activity component gains a mutex-protected GC maintenance record:

```text
GcMaintenanceState
  active_leases: usize
  revision: u64
  explicit_request: bool
  disposition: Idle | RetryRequired | Poisoned
  failures: durable ordered failure records
  pending_failure_reports: ordered unreported failure records
```

The parking generation remains a separate field in the same component. It may
advance conservatively; the GC maintenance revision may advance only when one
of the authoritative fields above changes. Readiness and settlement validate
the revision, never the parking generation and never a sampled collector
epoch, activity count, or pressure bit.

One owned `RuntimeGcActivityLease` follows this protocol:

1. acquire shared runtime mutation admission;
2. increment `active_leases` and the GC maintenance revision;
3. release admission and issue the ordinary activity wake;
4. run the potentially collecting heap entry, including all finalization;
5. obtain a non-panicking collector maintenance snapshot;
6. reacquire shared mutation admission;
7. publish success, retry-required, or poisoned disposition; update the
   failure ledgers; decrement `active_leases`; advance the revision; and
8. release admission and issue the ordinary activity wake.

The lease is unwind-safe. Its fallback `Drop` always retires the active count;
the potentially collecting facade catches an unwind long enough to publish
the collector's current disposition before either returning a structured
maintenance error or resuming an unrelated automatic-entry panic. The
collector receives no runtime callback.

Several leases may coexist. Readiness remains `Busy` until the last one
retires. Outcome publication derives retry state from the current collector
snapshot rather than assuming completion order: a concurrent retry may have
already consumed a failed attempt's pending batch before the original caller
records its historical failure.

## Entry Inventory and Classification

The finite authority surface is classified as follows:

| Entry family | Current `NoAuto` classification | Future classification |
| --- | --- | --- |
| `CoreValueFactory::with_runtime_value_access` | cannot collect; no lease | may elect on an `Automatic` heap and must use the leased facade |
| `CoreValueFactory::with_managed_values` | cannot collect; no lease | same leased facade as ordinary access if its heap may elect |
| evaluator, compiler, reflection, public `Values`, observer, cache, and net helpers | delegate to one of the two factory entries; no independent collector authority | inherit the factory entry's immutable policy classification |
| recursive same-heap entry | cannot independently elect | remains behind the facade; an initial implementation may conservatively share/count the outer lease rather than exposing a bypass |
| `EvaluationRuntime::collect_managed_for_maintenance` | explicit collection; currently I11-only serial seam | class 3, always leased before `collect_full` |
| explicit runtime maintenance request | request-only; cannot collect | no lease; publish the runtime request under shared admission and wake |
| allocation-pressure request on a `NoAuto` heap | advisory collector hint, not runtime work | remains observational until an embedding maintenance policy elects service |
| aggressive pre-outer-entry verification | can collect despite immutable `NoAuto` | class 2 and leased whenever enabled |
| runtime construction/canonical cache initialization | fixed `NoAuto`; cannot collect | a future automatic constructor must install policy and activity authority before its first potentially collecting entry |
| isolated `CoreValueFactory` and `glam-gc::Heap` fixtures | no `EvaluationRuntime` readiness exists | remain explicitly isolated/test-only and do not manufacture runtime leases |

The private value-domain source boundary has exactly two direct
`Heap::with_mutator` calls, one direct `Heap::collect_full` call, one test-only
`Heap::request_collection` precursor, and one `Heap::new_with_policy` call.
The I12 source latch owns these counts and requires the planned production
request to replace or reuse that precursor. Adding another production entry
without classifying it is a failing change.

The distinction between pressure and an explicit request is deliberate.
Ordinary `NoAuto` allocation can raise the collector's pressure latch while
readiness is being inspected. Treating that advisory bit as authoritative
would require every allocation region to pay for runtime activity even though
it cannot collect. Instead, the public/runtime request has its own
authoritative bit. Pressure remains a maintenance-policy input sampled at a
chosen boundary; it does not by itself prevent quiescence.

## Readiness and Settlement Projection

`RuntimeReadinessStamp` gains `gc_maintenance_revision`. Under exclusive
admission, readiness applies this precedence:

1. one or more active leases: `Busy`;
2. poisoned heap disposition: `MaintenanceFailed`;
3. explicit request or pending finalizer retry: `MaintenanceRequired`;
4. otherwise, the existing coordinator/event `Busy`, `Ready`, or `Deadlocked`
   classification.

`MaintenanceRequired` and `MaintenanceFailed` carry retained runtime-local
snapshots and the maintenance revision, not raw heap handles. The former
distinguishes an explicit request from `RetryRequired`; the latter is terminal
for the current value domain. `pump_until_stable` may return before either
actionable disposition: maintenance is embedding-client work, not background
evaluation work.

Ready/deadlock settlement revalidates all three existing/current authorities:
coordinator work generation, observation epoch, and GC maintenance revision.
It also requires zero active leases and an idle maintenance disposition. A
lease admitted after the readiness observation therefore invalidates the
snapshot even if collection changes no semantic state.

## Finalizer Panic and Failure Policy

Any recoverable collector panic is represented as `RetryRequired`; it is never
anonymous `Busy`. For a finalizer panic, that state owns the collector's
inactive pending batch. A reversible trace panic has no finalizer batch but
retains the same explicit retry obligation. The failed attempt also appends
one durable `RuntimeMaintenanceFailure` with an owned Rust-side kind and
message. Failure construction must not require entering the damaged heap.

- A later successful collection clears `RetryRequired`, but does not erase
  the historical failure.
- Settlement moves an unreported failure into the ordinary one-shot pending
  report flow while retaining it in the report's full maintenance-failure
  collection.
- There is no separate `ack_error`-style maintenance operation in I12. Retry
  acknowledges the outstanding finalizer work; settlement/reporting
  acknowledges delivery of the diagnostic.
- Any maintenance failure independently contributes to batch failure policy,
  even if retry later succeeds.
- A permanently poisoned heap becomes `MaintenanceFailed`. The client emits a
  fallback host diagnostic and drops the runtime; it must not try to enrich
  that error by entering the poisoned value domain.

For an explicit maintenance call, a recovered collector panic becomes a
structured `RuntimeMaintenanceError` rather than escaping as an unclassified
panic. The error distinguishes a reversible collector panic, a finalizer panic
with pending obligations, and permanent poison. A future automatic entry
records the same durable state before preserving the original Rust unwind
contract. Arbitrary panics from the caller's regional operation do not become
GC failures when the collector snapshot is idle and usable.

## Request and Completion Linearization

The nonblocking runtime request performs `Heap::request_collection` and sets
the runtime `explicit_request` under shared mutation admission. It takes no
activity lease because it cannot collect. Its publication wake makes a stable
client observe `MaintenanceRequired`.

Successful maintenance clears every explicit request linearized before its
completion publication. A request serialized after that publication remains
latched. Requests arriving during finalization are therefore coalesced into the
active attempt rather than forcing a redundant second collection. On failure,
the request remains actionable together with `RetryRequired`.

Collector pressure raised by typed-run publication follows the separate
advisory rule above. Metrics expose pressure and collection results for host
policy and profiling, but neither pure Glam evaluation nor readiness can
observe them as semantic state.

## Required Implementation Evidence

I12A must force, rather than merely repeat, these orders:

- exclusive readiness admission immediately before a lease registration;
- lease registration after a readiness snapshot and rejection of that stale
  snapshot;
- a parked pump observing the generation before a blocked finalizer completes,
  with no lost wake after lease retirement;
- several concurrent leases, with `Busy` retained until the final retirement;
- successful collection, reversible trace panic, finalizer panic, successful
  retry, and permanent poison, each publishing exactly one selected state;
- request-before-completion coalescing and request-after-completion retention;
- current `NoAuto` pressure remaining advisory across ordinary outer entries;
  and
- future automatic/aggressive entry using the same facade without a collector
  callback.

The source-backed review latches are
`gc_activity_entry_inventory_is_complete`,
`gc_readiness_plan_has_one_authoritative_activity_source`, and
`pending_finalizer_batch_has_durable_nonbusy_disposition`.

## Decision

I12A.0 passes. I12A may implement explicit maintenance in small checkpoints.
I12B.0 remains blocked until that implementation and its post-phase review are
complete. No live heap changes policy, and no ordinary collection has been
enabled by this review.
