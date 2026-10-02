# Garbage Collector Aggressive Verification Closure — 2026-10-01

Baseline: `5759ee6d`, the completed GCI11R-002D.2h.4 source reconciliation
after the ordinary and aggressive workspace gates.

Status: complete. GCI11R-002 and I11D.1 are resolved. The private
`aggressive-gc-verification` repository mode completes the entire workspace
without changing production's immutable `CollectionPolicy::NoAuto`. Gate G3
remains closed: I11D.2 dynamic unsafe-boundary verification, I11D.3 static
closure audit, and I11D.4 certification are separate work.

Follow-up review separates the focused D.2h implementation accounting in
[`GarbageCollectorAggressiveD2h_2026-10-01.md`](GarbageCollectorAggressiveD2h_2026-10-01.md)
from the parent-plan and Gate G3 reconciliation in
[`GarbageCollectorGCI11R002Holistic_2026-10-01.md`](GarbageCollectorGCI11R002Holistic_2026-10-01.md).

## Scope and Method

This review closes the regional-value ownership remediation which began when
collection before every eligible outer runtime entry exposed stale managed
edges. It accounts for:

- the two production ownership repairs and the subsequent source-wide raw
  value and durable-owner migration;
- removal of implicit managed-edge duplication, equality, formatting, and
  identity operations;
- test fixture publication and schedule repairs found only by the complete
  aggressive workspace;
- exact source inventories after every dynamic repair; and
- ordinary, aggressive, profiling, formatting, and lint closure gates.

The review distinguishes source evidence from dynamic evidence. Exact
inventories establish which operations and owners exist. Aggressive collection
establishes that those owners survive the most hostile supported bootstrap
admission schedule. Forced barriers, hooks, or explicit serial-order loops
establish concurrency claims; repeated success is not evidence for an
ordering.

## Accepted Surfaces

| Surface | Final source authority | Accepted contract |
| --- | --- | --- |
| Raw core values | 518 occurrences; fingerprint `9_495_114_017_072_832_932` | 485 regional access-qualified functions, 28 collector-only primitives, and five regional representation aliases. There is no violation or pending remediation assignment. |
| Persistent managed edges | 873 occurrences; fingerprint `13_687_440_433_971_841_379` | 148 production typed, 36 production erased, 675 test typed, and 14 test erased. Typed edges use explicit access-qualified duplication/identity; erased identity remains collector-private. There is no defect or pending classification. |
| Runtime-root publication | 272 occurrences; fingerprint `11_317_655_089_926_401_572` | Publication uses the two canonical runtime-root constructors. There is no defect or nested scoped-factory construction. |
| Mutator introduction | 534 occurrences; fingerprint `852_771_498_578_214_529` | Direct mutator admission is confined to two higher-ranked gateways in `src/core/managed.rs`. The reviewed call graph contains 533 access-gateway and 14 construction-gateway uses. |

The durable-owner, containment, active-RAII, callback-capture, machine-state,
recursive-identity, runtime-cache, compiler-boundary, and resolved-call
inventories also close without a defect or pending production disposition.
`cargo test -q inventory` passes all 121 library inventory tests and the eight
integration tests selected by that filter.

## Semantic Decisions Preserved

Raw `core::Value` is regional. Code may pass or return one while matching
`RuntimeValueAccess` or `EvaluationValueAccess` remains live, or store it as
the unobserved payload of an exhaustively traced managed owner. Crossing an
orchestration, callback, wait, or public boundary requires the appropriate
durable root; opening a later region to rescue a previously unrooted edge is
not an ownership model.

`Gc<T>` and the three managed facades expose no ambient `Copy`, `Clone`,
`PartialEq`, `Eq`, `Debug`, or unqualified pointer-identity operation.
Duplication, semantic comparison, identity, and recursive diagnostics are
explicit operations under matching access. `ErasedGc` remains the private
collector address identity.

Structured evaluation failures retain their canonical
`Arc<EvaluationFailure>` identity. Rust `Display` provides only the settled
edge-free compatibility classification; Glam diagnostic projection remains
an explicit access-qualified operation which preserves the structured value
and rendering policy.

Test-only exact roots are real fixture ownership, not production exceptions.
When a fixture observes a lazy or promise after cancellation, abandonment,
settlement, or result retirement, it retains that exact managed identity from
the construction region. Aggressive admission may reclaim an unrooted slot
before a later explicit collection report, so reclamation fixtures assert
liveness and eventual collection invariants rather than incidental ownership
of the collection epoch which first reclaimed the slot.

The aggressive feature remains verification-only. It collects before every
eligible outer entry into a complete production runtime value domain, does not
alter `NoAuto`, and is not a supported embedding policy or a claim of automatic
production collection.

## Defects and Drift Accounted For

The remediation found substantive production defects rather than only stale
tests: deferred evaluator work and closed compiler evaluation had raw-value
handoff gaps, list-effect checkpoints omitted managed list-thunk edges, and a
test-facing lowered-source copy duplicated a root's definitions outside the
accepted ownership boundary. Those representations now retain or publish one
exhaustive owner.

It also found fixture defects: several helpers allocated a lazy, promise,
effect, metadata carrier, application, reflection gate, or core net in one
region and rooted it only after entering another. Those helpers now allocate
and publish their final root in one region. Shared test heaps were replaced by
private domains only where the fixture itself performed collection; this
removes cross-test lifetime interference without weakening production
semantics.

Schedule fixes preserve the asserted ordering. One-shot finalization probes
are installed after setup, the promise-follower race is forced in both serial
orders, claimed-pair selection waits for an actually ready pair, and the
existing production-shaped matrix uses barriers or exact hooks rather than
test repetition.

The complete aggressive workspace is intentionally expensive. Its five
direct-assembly subprocesses remained compute-bound and eventually completed;
their roughly four-hour aggregate gate is a future verification-performance
target, not evidence of deadlock and not a correctness blocker.

## Verification Record

The final closure passed:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
scripts/check-interaction-net-profiling.sh
cargo test --workspace -q --features aggressive-gc-verification
cargo test -q inventory
```

The ordinary workspace reported 1,882 library tests passed and two ignored,
followed by every integration, executable, sample, invalid-sample, macro, and
collector target. The aggressive workspace reported 1,885 library tests
passed and two ignored, followed by the same target families. The profiling
script passed each named interaction-net accounting fixture. The final
inventory rerun moved no accepted count or fingerprint.

The complete aggressive gate, not an isolated or repeated failing test, is the
authoritative GCI11R-002 result. No test was skipped or assertion weakened to
make that mode green.

## Nested Plan Disposition and Remaining Work

The persistent-edge migration is complete through P4:

- P3's parent raw-value interlock has zero violation and defect entries;
- P4A and P4B removed ambient duplication and observation from `Gc<T>`, the
  managed facades, and raw semantic carriers; and
- P4C's negative trait contracts and source manifests are active.

P5C parent reconciliation is complete through this review. P5A's ordinary and
aggressive behavioral matrix is complete, but its focused Miri obligation is
not newly satisfied by this closure and remains part of I11D.2. P5B's explicit
release-cost/code-generation review likewise remains open; existing pointer
layout and root-registration tests do not substitute for that final audit.

Follow-up: I11D.2 completed both remaining obligations on 2026-10-02. The
release-cost evidence and continuing code-generation latch are recorded in
[`GarbageCollectorI11D2PersistentEdgeCost_2026-10-02.md`](GarbageCollectorI11D2PersistentEdgeCost_2026-10-02.md).

The next Gate G3 work is therefore I11D.3, not more GCI11R-002 fixture
migration. Moving collection, lifetime-branded managed pointers, concurrent
collection, automatic production policy, and broader diagnostic rendering
cleanup remain their existing deferred plans.
