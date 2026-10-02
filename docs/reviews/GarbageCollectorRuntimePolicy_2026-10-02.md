# GC Runtime Collection-Policy Decision — 2026-10-02

Related plan:
[`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md),
Phase I12B.0.

Decision: **permanently manual runtimes**. Every Glam runtime heap continues
to use immutable `CollectionPolicy::NoAuto`. Collector pressure will be
promoted to an explicit runtime maintenance obligation only at the stable pump
boundary selected by I12B; ordinary mutator entry never services production
pressure.

Status: **complete; I12B.0 passes.** This review selects the construction
policy and rewrites I12B. It does not change a live heap, enable collection,
or add synchronization to ordinary value access.

## Decision Summary

Automatic collector election is the wrong bootstrap default for Glam's
current runtime boundary. A potentially collecting outer value entry must
publish a runtime activity lease before collector election and retire it after
collection and finalization. With the current I12A protocol, doing that for
every outer value-access region would add two activity-state publications and
wakes plus a collector outcome observation to a pervasive evaluator/compiler
path. The collection itself would also begin at whichever helper happened to
open the next mutator, rather than at a runtime-selected maintenance boundary.

Manual policy preserves the desired separation:

- allocation records cheap collector-local pressure without runtime work;
- ordinary value access remains the current direct `NoAuto` mutator entry;
- the runtime pump may promote pressure while establishing a stable instant;
- readiness then exposes the existing explicit maintenance snapshot; and
- collection, failure retention, retry, and poison use I12A's one supported
  service protocol.

This does not make pressure optional or leave it permanently invisible. I12B
must add stable-boundary promotion for production pumps. A client which wants
different placement may continue to use `request_managed_collection` and
`service_managed_collection` directly. Continuously busy runtimes can still
defer STW service; that accepted progress limitation belongs to the later
concurrent-collector plan rather than to hot-path automatic entry.

## Construction Inventory

There is one production value-domain heap constructor:
`CoreValueFactory::new`, which selects `CollectionPolicy::NoAuto` before
canonical values are initialized. The higher-level construction paths are:

| Construction path | Policy selection |
| --- | --- |
| `EvaluationRuntime::new` | delegates to `EvaluationRuntime::with_conflict_analysis` |
| `EvaluationRuntime::with_conflict_analysis` | constructs one `CoreValueFactory` and therefore one `NoAuto` heap |
| `AssemblerBuilder::default` / `AssemblerBuilder::new` | constructs an `EvaluationRuntime`; no independent policy |
| `AssemblerBuilder::evaluation_runtime` | accepts an already-constructed runtime and cannot reinterpret its policy |
| batch configuration in `src/bin/glam/configuration/mod.rs` | constructs an `EvaluationRuntime`; no independent policy |
| crate-local compiler/evaluator test factories | call the same `CoreValueFactory::new` and remain `NoAuto` |
| direct `glam_gc::Heap` fixtures | collector-local, outside runtime readiness; each test selects or verifies its own policy |

The public runtime API deliberately does not expose a collection-policy
parameter. Glam owns the runtime policy, while embedding clients own explicit
maintenance placement. `glam_gc::Heap::new` may retain its collector-library
default of `Automatic`; it is not a Glam runtime constructor. Aggressive GC
verification also retains an immutable `NoAuto` heap and uses a separate
test/feature-only pre-entry collection hook.

No configuration, reflection profile, environment entry, or assembler option
selects collection policy. There is no live policy setter and no
`NoAuto`-to-`Automatic` transition.

## Operational and Performance Evidence

I12A established the semantic and concurrency evidence needed by either
choice: full production-graph preservation, exact pressure latching,
unwind-safe activity, forced readiness/finalizer schedules, retry, and poison.
The pressure fixture reaches the collector's real 112-run (7 MiB) initial
high-water mark, proves ordinary `NoAuto` entries leave the request pending,
and proves explicit service consumes it.

As a release-mode representative baseline, the direct-assembly Hello World
sample produced the same 166-byte ELF (SHA-256
`e18df1ef13e32df92ca590919decaf97ed1664c60df7747dd49c1e71577b1e01`)
on each run. Three warm invocations took 3.523-3.546 seconds wall time on the
review host. These numbers are descriptive rather than a stable performance
threshold.

There is no honest automatic-runtime comparison yet: the production path is
intentionally absent. The source shape nevertheless establishes a material
cost difference. Ordinary `NoAuto` entry compiles to an inline direct call.
The readiness-safe automatic prototype would have to take and retire an I12A
activity lease around every outer access, publish wakes, and obtain a
post-entry collector snapshot even when pressure is absent. Enabling that path
without representative evidence would spend synchronization on the common
case to place an uncommon STW pause less deliberately.

Manual policy also aligns better with the future concurrent collector. The
current automatic outer-entry election does not solve multi-heap or
continuously-overlapping-mutator starvation; the dedicated concurrent-GC plan
owns that progress problem. Avoiding a temporary pervasive lease protocol now
reduces migration work later.

## Rejected Alternatives

### Automatic runtimes by default

Rejected for the bootstrap. It offers prompt pressure service with little
runtime policy code, but requires authoritative activity around all possible
entry elections and makes pause placement an accident of the next value
access. Its benefit does not justify the pervasive coordination cost while a
reviewed explicit service already exists.

### A live or pressure-selected hybrid

Rejected. Heap policy is immutable, and changing it in response to pressure
would make the safety and readiness obligations depend on state observed too
late. Pressure is instead promoted to ordinary explicit maintenance state at
a stable runtime boundary.

### Public per-runtime policy configuration

Rejected for now. It would expose collector strategy through a bootstrap API,
double the production entry matrix, and still require the automatic hot-path
protocol. Direct collector fixtures remain sufficient to verify both
`glam-gc` policies.

## I12B Implementation Consequences

I12B is now a manual-policy implementation phase:

1. latch the complete constructor inventory and the single immutable
   `NoAuto` selection;
2. let `pump_until_stable` promote observed collector pressure into I12A's
   authoritative explicit-request state only after it has reached a stable
   pump boundary under runtime admission;
3. leave `readiness` observational and let it return the resulting
   `MaintenanceRequired` snapshot;
4. exercise direct service, snapshot service, batch service, retry, and the
   pressure-promotion boundary without adding collection to ordinary value
   entry; and
5. repeat representative output and pressure/reclamation checks before the
   post-I12 review.

The implementation must treat promotion as a runtime operational transition,
not as Glam semantics. A pressure request racing with a completed collection
may cause one conservative extra maintenance attempt, but may not be lost.
Failure remains actionable through the existing I12A disposition and ledgers.

## Gate G4 and Forward Work

Gate G4 requires documentation and source inventories to describe
production as permanently explicit `NoAuto` maintenance. It does not require
automatic outer-entry collection. Before G4, I12B must prove that the stable
pump boundary makes real pressure actionable and that no production mutator
entry can elect collection.

Concurrent marking, concurrent run retirement, moving collection,
generational policy, and alternate user-directed normalization/maintenance
annotations remain deferred. A future reviewed architecture may replace this
bootstrap policy, but it must do so as a new construction contract rather than
as a live heap transition.

## Verification Record

The review is latched by
`runtime_gc_policy_review_inventory_is_complete`,
`runtime_gc_policy_plan_has_no_live_transition`, and
`runtime_gc_policy_review_links_selected_plan_and_completion_gate`.

Fresh checks passed on 2026-10-02:

```text
cargo fmt --check
    passed

cargo clippy --all-targets --all-features -- -D warnings
    passed

cargo test -q
    root library: 1,903 passed, 2 intentional stress ignores
    every auxiliary target passed

scripts/check-interaction-net-profiling.sh
    21 named profiling regressions passed

cargo build --release -q
    passed (one pre-existing non-all-features unused-variable warning)

release direct-assembly sample
    166 bytes; stable SHA-256 recorded above
    3.523, 3.546, and 3.529 seconds wall time
```

No concurrent claim relies on repetition: I12B.0 changes documentation and
source inventories only. I12B's implementation phase owns the forced-order
pressure-promotion schedules.
