# Glam GC Integration Phase I11 Review — 2026-09-11

Baseline: `6746551`, the completed I11A-I11C implementation. Gate G2 was
certified independently at `585cfec`; this review covers the production
collection work from that gate through I11C.

Status: review complete; Gate G3 remains closed. The implemented collection,
ownership, finalization, request-coalescing, and runtime-retirement boundaries
remain coherent, and no production tracing defect was found. Three
verification gaps must be closed before I11D can certify the boundary:
GCI11R-001 does not yet force the collector to reach its active-mutator wait,
GCI11R-002 leaves the promised repository-wide aggressive mode unavailable,
and GCI11R-003 does not dynamically prove the absence of managed allocation
during passive finalization. Production remains `CollectionPolicy::NoAuto`.

## Scope and Method

The review compared I11 with the collector implementation plan, integration
plan, roadmap invariants, ownership ledger, Gate G2 review, I11B/I11C reviews,
and current source. It audited:

- the private production maintenance route and immutable heap policy;
- every I11 production root, managed-cycle, compatibility-container,
  interaction-net, callback, opaque, and external-owner outcome;
- worker managed-access and host-work boundaries;
- finalizer admission, passive managed destruction, external active-owner
  retirement, and collection-request ordering;
- runtime/value-domain retirement before and after collection;
- the deterministic hook boundary and whether each concurrency assertion is
  established by an observed ordering rather than scheduler luck;
- newly introduced unsafe, trace, mutation, lock, callback, and re-entry
  surfaces; and
- whether I11D and I12-I13 still describe executable work against the current
  implementation.

The review treats source inventories as change detectors and focused
collection fixtures as semantic evidence. Neither substitutes for the other.
Repeated success is not evidence for a concurrency ordering.

## Plan-to-Implementation Accounting

| Checkpoint | Current disposition |
| --- | --- |
| I11A / Gate G2 | Complete and independently certified. The four managed families, all compatibility edges, every durable root surface, and all external-owner exceptions have source-backed records and isolated reclamation evidence. |
| I11B.1 | Complete. `EvaluationRuntime::collect_managed_for_maintenance` is crate-private, delegates to the matching value domain, and does not alter immutable `NoAuto` policy. |
| I11B.2 | Complete. The production serial fixture preserves compilation results, reflection roots, queued output, structured diagnostics, settlement, readiness stamps, observation epochs, and net topology across controlled collections. |
| I11B.3 | Complete. Production runtimes reclaim recursive identity and compatibility-container cycles, while reviewed external owners retain and retire according to their explicit lifecycle. |
| I11B.4 | Complete. The dated I11B review and routine checks reconcile the serial boundary with Gate G2. |
| I11C.1 | Implementation behavior is plausible and lower-level collector tests cover exclusive admission, but the production worker fixture does not yet latch that its collector reached the blocked admission state. GCI11R-001 keeps this verification checkpoint open for Gate G3. |
| I11C.2 | Passive shell finalization preserves runtime/coordinator/event state and external payload ownership. Its no-managed-allocation claim needs exact slot accounting under GCI11R-003. |
| I11C.3 | Complete. The Finalizing-phase probe establishes durable finalizer work without holding a collector component mutex; a host callback issues a nonblocking request, successful completion coalesces it, and a later explicit pass advances exactly once. |
| I11C.4 | Complete. Public values do not retain or revive a retired value domain, and the one-shot finalizer probe has an independent RAII release test. |

## Ownership, Synchronization, and Semantic Review

The private maintenance call reaches exactly the runtime's value-domain heap.
It does not infer quiescence, hold runtime mutation admission, publish semantic
observation state, or expose collection to embedding clients. That is the
correct I11 boundary: I12 must add authoritative runtime operational activity
before ordinary maintenance can call the same collector concurrently.

The managed worker enters through `EvaluationPollContext::with_value_access`.
The worker therefore holds ordinary mutator admission only for the bounded
semantic quantum, and the synchronous collector cannot become exclusive until
that region closes. The implementation has the required exclusion; the open
finding concerns only whether the production test observes the collector at
that wait boundary.

The host-worker fixture deliberately pauses outside managed access. Collection
may proceed beside that scheduler-owned work, while the runtime remains
`Busy`. Finalization of a `ManagedValueNode` containing an opaque variant drops
only its passive scalar `ExternalOwnerHandle`. The active Rust payload remains
in the external registry until an explicit unlocked drain. No finalizer
receives a runtime, heap, evaluator, callback, diagnostic route, or event
publisher.

The one-shot Finalizing probe is test-only. The collector removes the probe
from its installation slot, releases that slot's mutex, and waits only on the
probe-local condition variable while retaining its normal finalizer mutator.
The output callback used by I11C.3 performs no managed access; it reaches only
the nonblocking atomic request path. This preserves the rule that finalizers do
not trigger runtime callbacks or recursive worker entry.

The I11 diff adds no unsafe block or new production trace/mutation gateway.
All graph mutation remains behind the already reviewed owner/set and net
transition gateways. Collection changes allocation topology and operational
collector statistics, but not Glam evaluation results. I11B's unchanged
readiness-stamp assertion applies only to the current private stable-boundary
seam: I12's future authoritative activity lease will intentionally invalidate
or defer a concurrent readiness observation while maintenance is active.

## Findings

### GCI11R-001 — The production worker/collector ordering is not fully latched

**Severity:** high verification gap; blocks Gate G3.

**Status:** resolved by I11D.0 on 2026-09-11.

`collection_interleaves_with_worker_quantum_without_lost_work` correctly waits
until the worker is inside real managed access. Its collector thread then sends
`collector_started` immediately *before* calling
`collect_managed_for_maintenance`. Receiving that message proves only that the
thread was scheduled. The main thread can observe an empty result channel and
release the worker before the collector has requested or waited for exclusive
admission. The fixture can therefore pass without exercising the advertised
contention schedule.

The collector already has authoritative coordinator state for a blocked
synchronous collector, but the production fixture cannot observe it. Add a
private one-shot admission-wait probe under `deterministic-test-hooks`. It must
announce only after the synchronous target exists and the collector has found
an active outer mutator preventing election; it must not pause while holding
the coordinator mutex. The production test then waits for that observation,
proves no collection result exists, releases the worker, and observes both the
collection report and normal task result. Add a baseline collection before the
worker is scheduled so the named before/during/after matrix is literal.

Use bounded waits only as harness watchdogs. The proof remains the probe's
state transition, not elapsed time or repeated runs.

The one-shot probe now announces after target reservation and an authoritative
`Ordinary + active_outer_mutators != 0` observation, outside the coordinator
mutex. The production fixture waits for that transition, observes the absent
result, releases the worker, and checks exact collection epochs immediately
before and after both disputed boundaries. This remains exact when the
repository aggressive mode adds unrelated pre-entry collections.

### GCI11R-002 — Aggressive collection is not a repository-wide test mode

**Severity:** high verification gap; blocks Gate G3.

**Status:** implementation present; aggressive suite remains failing and the
finding remains open.

`Heap::enable_collection_before_outer_entry` is a sound heap-local
deterministic hook, and focused collector/runtime tests prove its outer-versus-
recursive-entry behavior. Nothing enables it for every production
`RuntimeValueDomain` constructed by a repository test run. The only root
wrappers are `#[cfg(test)]` helpers, so I11D's promised full repository suite
under aggressive debug collection is not currently an executable command.

Add one private root-crate verification feature which forwards the collector's
`deterministic-test-hooks` feature and enables aggressive pre-entry collection
when a production runtime value domain is constructed. The mode must remain a
compile-time/test invocation choice, preserve immutable `NoAuto`, and expose no
supported embedding API. Run the complete workspace both normally and with
that feature. Assertions about exact operational collection epochs may be made
mode-aware, but semantic, ownership, and schedule assertions must not be
weakened or skipped.

I11D.1 added `aggressive-gc-verification`, enabled it after complete production
runtime construction, retained `NoAuto`, and made cross-heap nested entry defer
to the next eligible outer entry. A focused fixture proves automatic enablement
and policy preservation. The first complete-workspace command exposed widespread
stale-edge failures instead of passing. One production issue was repaired:
closed runtime-cache construction now retains one outer access region until
the completed cache family's declared roots exist, and the focused compiler-
cache lifecycle test passes under the mode. At least one remaining source-
compilation/reflection path still deterministically reaches
`collector edge does not identify an allocated value`; several test helpers
also construct a raw managed identity in one region and root it only in a later
region. These require a regional-handoff audit beyond the test-only remediation
assumed at review time. The failing feature run is now the authoritative
reproducer and Gate G3 remains closed.

### GCI11R-003 — Passive-finalizer allocation absence is not measured exactly

**Severity:** medium verification gap; blocks Gate G3's finalization claim.

**Status:** resolved by I11D.0 on 2026-09-11.

`passive_finalization_produces_no_runtime_work` proves that diagnostic counts,
observation epoch, coordinator inventory, readiness, event contents, and
finalizer activity remain unchanged. It also proves assigned-run pressure does
not grow. Run pressure is coarser than allocation: a finalizer could allocate a
slot in an existing run without changing that statistic.

Add a deterministic-test-only exact allocated-slot snapshot which scans the
existing allocation bitmaps without adding a production counter or allocation-
path cost. Around the latched passive-finalization fixture, require:

```text
allocated_after + report.reclaimed_slots == allocated_before
```

With managed access excluded for the fixture's host worker, this proves that
collection retired the reported slots and introduced no replacement managed
allocation. Keep the structural `ManagedDropRecord` and active-owner
inventories: exact dynamic accounting complements rather than replaces their
compile-time change detection.

The deterministic hook now atomically counts valid allocation bits across
attached runs at a fixture-established stable boundary and rejects pending
detached finalizers. The passive-finalization test proves the exact equation
above in both ordinary and aggressive focused runs.

### GCI11R-004 — Gate naming and current architecture text lag I11C

**Severity:** low documentation drift; resolved by this review.

The roadmap called Gate G3 “full collection enabled,” although passing it only
certifies collection and authorizes I12 to design and implement runtime
maintenance. The evaluation architecture also stopped at I11B's serial
boundary and omitted I11C's private concurrent schedule fixtures. Those texts
now distinguish certification from later enablement.

## I11D Remediation and Certification Sequence

I11D is partitioned so expensive dynamic-tool runs happen only after the
deterministic verification gaps are closed:

1. **I11D.0 — Post-I11 remediation.** Close GCI11R-001 and GCI11R-003, add the
   exact pre/during/post worker chronology, and rerun all focused I11 tests.
2. **I11D.1 — Aggressive repository mode.** Close GCI11R-002, run the complete
   repository in ordinary and aggressive modes, and retain a focused assertion
   that the verification mode never changes `NoAuto`.
3. **I11D.2 — Dynamic unsafe-boundary verification.** Run named focused Miri
   tests for roots, tracing, mutation transitions, allocation, collection,
   finalization, and the deterministic hooks. Run AddressSanitizer and
   ThreadSanitizer over supported focused targets. Record unsupported
   tool/target combinations explicitly; ordinary repetition is not a
   substitute.
4. **I11D.3 — Static closure audit.** Reconcile every unsafe site, trace edge,
   mutation gateway, managed entry, lock/wait boundary, finalizer, and active
   external owner against its authoritative source inventory and the changes
   since Gate G2.
5. **I11D.4 — Gate G3 certification.** Publish the dated gate review, record
   exact commands and results, close every I11 finding, and only then mark I11
   and Gate G3 complete.

## Future-Phase Review

- **I12A.0 remains necessary.** I11's private maintenance seam is not runtime
  readiness authority. Routine maintenance needs the planned operational-
  activity lease, wake protocol, and durable finalizer-panic disposition.
- **I12 must revise the readiness-stamp expectation deliberately.** Private
  I11 collection at a caller-owned stable boundary is invisible to semantic
  observation. A future activity lease is itself authoritative operational
  state while active and may stale an overlapping readiness snapshot without
  making collector timing a Glam semantic.
- **I12's entry inventory must include the aggressive mode.** It is the most
  demanding outer-entry schedule even though it remains verification-only.
- **I13 remains cleanup, not a correctness phase.** It may remove compatibility
  ownership only after Gate G3; a missing trace edge, first-owner publication,
  or mutation gateway reopens the phase which introduced it.
- **The concurrent-collector plan remains deferred.** I11 certifies the
  stop-the-world `NoAuto` design and makes no progress-fairness claim under
  continuously overlapping mutators.

## Verification Baseline

At review time:

- `cargo fmt --check` passes;
- the focused collector suite with `deterministic-test-hooks` passes (196
  passed, 2 ignored);
- the ordinary repository suite passes (1,459 main library tests plus all
  auxiliary targets);
- `git diff --check 585cfec..6746551` passes and the I11 diff adds no unsafe
  block;
- nightly Miri is installed and invocable as `cargo +nightly miri`; and
- the installed nightly compiler exposes `-Z sanitizer`.

Those green results are the I11D starting baseline, not Gate G3 evidence for
the three missing schedules/modes above.
