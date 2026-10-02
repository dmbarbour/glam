# Glam GC Gate G4 and Post-I13 Review — 2026-10-02

Gate G3 baseline: `26c86f71`, certified on 2026-10-02.

I13 implementation baseline before certification: `13b0236e`.

Status: **Gate G4 passed; collector integration I0-I13 is complete.** The
final ownership inventory, cleanup, documentation, source audits, collection
fixtures, and repository checks agree. Recursive production identities are
exact managed edges; every remaining reference-counted or copied identity has
a reviewed structural, external-root, scheduler, host, routing, or diagnostic
role. This closes the initial Glam integration of the non-moving stop-the-world
collector. It does not complete collector stress/tuning C7-C8 or enable
automatic, concurrent, moving, or generational collection.

## Reviewed Scope

I13 was deliberately a delta review rather than another ownership migration:

- I13A reused the authoritative recursive-identity, durable-root,
  containment, active-owner, persistent-edge, raw-value, and opaque-family
  inventories and assigned a stable disposition to every retained owner,
  adapter, gate, provenance copy, and drop record;
- I13B removed stale allowances and transition terminology, made the direct
  allocation-scope bridge test-only, and retained every semantically relevant
  owner and compatibility edge;
- I13C documented the implemented edge/root boundary, regional allocation,
  safepoints, mutation gateways, passive destruction, explicit maintenance,
  and the accepted `NoAuto` starvation boundary; and
- I13D reran the source and dynamic delta audits, repaired one test-domain
  isolation defect, and closed this gate.

The detailed ownership disposition remains in
[`GarbageCollectorI13CleanupInventory_2026-10-02.md`](GarbageCollectorI13CleanupInventory_2026-10-02.md).

## Ownership and Representation Decision

No recursive production identity remains under `Arc`. `LazyValue`,
`PromisedValue`, and `CoreRuntimeNet` each carry one exact managed edge.
Durable Rust boundaries use registered roots. Remaining `Arc` values provide
immutable structural sharing, edge-free scheduling/notification state, host
resources, or the generic non-core net API; removing them would add eager
copying, require a mutator under coordinator locks, or erase a genuine
external owner.

Copied runtime IDs and weak observers remain only where dead-runtime
validation, access-free routing, work indexing, or diagnostics must happen
without opening a mutator. The compatibility value visitor remains the exact
bridge through immutable list, dictionary, function, failure, and related
shells until Value Representation Refinement replaces those representations.
`ManagedDropRecord` remains mandatory collector-admission evidence because
`Trace` does not constrain destructor behavior.

The I13B direct-gateway cleanup is intentional drift from Gate G3's count:
production now has one direct higher-ranked collector admission,
`RuntimeValueAccess`; the direct allocation-scope bridge exists only for
collector fixtures. No production nested mutator introduction or pending
exception exists.

## Findings and Resolutions

### GCI13R-001 — Explicit collection of the shared test domain was unsafe

**Resolved.** The first parallel library audit exposed a managed pointer which
had entered finalization while another test still used it. The collector was
correct: several collection-sensitive fixtures used the process-wide
`test_value_factory()`, whose purpose is to amortize compiler-value setup among
otherwise non-collecting tests. Explicitly collecting that domain could
reclaim another parallel test's unrooted compatibility values.

The repair is structural rather than probabilistic:

- `private_test_value_factory()` names the only fixture shape authorized for
  explicit collection;
- `collect_managed_for_test()` deterministically rejects the process-wide
  shared domain;
- every discovered collection-sensitive macro, compiler, evaluator,
  reflection, search, store, and lifecycle fixture now owns a private domain;
  and
- private evaluator fixtures use matching-domain constructors and observers
  instead of hidden helpers backed by the shared factory.

A complete serial run with the assertion enabled inventoried every explicit
collection site: 1,907 tests passed, two intentional stress tests were
ignored, and the only four failures were the subsequently repaired inventory
latch plus three additional shared-domain collectors. The normal parallel
library suite then passed all 1,911 enabled tests. Repetition is not the proof;
the collection-gateway assertion permanently forbids the invalid schedule.

### GCI13R-002 — The release mutation gateway emitted a configuration warning

**Resolved.** In optimized builds without deterministic hooks, the
`with_edge_state_transition` owner exists only as safety authority and debug
validation. Its binding now reflects that role without changing layout,
generated behavior, or the future barrier contract. A fresh release check is
warning-free.

### GCI13R-003 — No ownership or semantic cleanup was deferred

**Confirmed.** I13A found no removable owner whose replacement was equally
cheap and available at the same boundary. I13B consequently did not disguise
representation work as cleanup. All retained adapters have a named current
role or a named later owner: aggregate-shell replacement belongs to Value
Representation Refinement; starvation-free/concurrent progress belongs to the
concurrent collector plan.

## Static and Dynamic Closure

The current quantitative source latches pass:

| Ledger | Current accepted surface |
| --- | --- |
| raw `core::Value` APIs | 518 total: 485 regional access, 28 collector primitives, five regional representations |
| persistent managed edges | 884 total: 148 production typed, 36 production erased, 686 test typed, 14 test erased |
| runtime-root publication | 272 exact occurrences |
| mutator introduction | 537 exact occurrences; one direct production admission, zero production nested or pending entries |

All 129 selected inventory tests pass. The durable-owner, containment,
active-owner, recursive-identity, cache, compiler-boundary, WHNF, resolved-call,
entry-policy, and Gate G2 composition latches remain closed. Focused dynamic
closure passes 23 managed-collection tests and seven named cycle-reclamation
tests.

Gate G3's complete aggressive workspace, supported Miri, ASan/LSan, and TSan
matrices remain applicable. I13 changes no production unsafe operation, trace
edge, mutation authority, managed representation, root lifecycle, finalizer,
or scheduler semantics. I12 added reviewed maintenance coordination rather
than graph representation. Gate G4 therefore reuses those expensive complete
dynamic results and freshly reruns the all-feature compiler, exact source
inventories, ordinary complete workspace, and the feature-enabled production
aggressive-entry smoke test. This is the recorded semantic rerun policy, not a
substitute for a failed dynamic target.

## Verification Record

Fresh checks on 2026-10-02 passed:

```text
cargo fmt --check
    passed

cargo clippy --all-targets --all-features -- -D warnings
    passed

cargo check --release -q
    passed without warnings

cargo test -q
    root library: 1,911 passed, 2 intentional stress ignores
    every auxiliary target passed

cargo test --workspace -q
    root and auxiliary targets passed
    glam-gc library: 204 passed, 2 intentional ignores
    seven Loom models and all remaining collector/doc targets passed

cargo test -q --lib inventory
    129 passed

cargo test -q --lib managed_collection_tests
    23 passed

cargo test -q --lib cycle_is_traced_and_reclaimed
    7 passed

cargo test -q --features aggressive-gc-verification --lib \
  repository_aggressive_mode_enables_each_production_runtime
    1 passed

scripts/check-interaction-net-profiling.sh
    all 21 named profiling regressions passed
```

The release direct-assembly Hello World sample remains 166 bytes with SHA-256
`e18df1ef13e32df92ca590919decaf97ed1664c60df7747dd49c1e71577b1e01`.
This is output-equivalence evidence, not a performance threshold.

## Gate Decision and Forward Work

Gate G4 passes. The integration plan is complete: one `EvaluationRuntime`
owns one managed value domain; recursive values, roots, workers, reflection,
interaction nets, explicit maintenance, pressure promotion, and teardown share
one reviewed ownership and safepoint model.

Production heaps remain immutable `CollectionPolicy::NoAuto`. Collection is
explicit and pressure is promoted only at the stable pump boundary. The
following remain deliberately separate work:

- collector C7/C8 stress, metrics, and tuning;
- Value Representation Refinement and direct managed container spines;
- concurrent marking, delayed logical sweep, and epoch-safe run recycling;
- moving/generational collection, weak references, and ephemerons; and
- the tentative lifetime-branded scoped-pointer experiment.

Those items may build on Gate G4; none is required to make the completed
initial integration sound.
