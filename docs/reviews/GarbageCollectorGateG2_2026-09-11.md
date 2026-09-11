# Glam GC Gate G2 Audit — 2026-09-11

Baseline: `4da3849`, the completed I10D and post-I10 integration boundary.
The I11A certification latches and documentation corrections described below
follow that baseline.

Status: complete. Gate G2 passes. The production value graph has no unmatched
graph-bearing field, incomplete managed-family record, or unclassified
external owner. Production remains `CollectionPolicy::NoAuto`; this gate
authorizes I11B's controlled full-production-graph tests, not collection in
ordinary execution.

## Scope and Method

Gate G2 asks whether a forced full collection can be introduced as a
controlled production-runtime test without relying on a conservative scan or
an unrecorded Rust owner. It does not itself run that collection. This audit:

1. independently composed the final source inventories for values, exact
   traces, registered roots, closures, opaque/type-erased families, caches,
   persistent collections, interaction nets, runtime owners, and callbacks;
2. reconciled those inventories with one complete stable ledger record for
   each production managed allocation family;
3. rechecked private regional allocation, exact first ownership, and the
   forced former-publication-gap fixtures;
4. rechecked every production trace, managed/opaque admission, downcast,
   mutation gateway, and I3 mutator/callback boundary; and
5. repeated the isolated family reclamation and collector layout/class suites
   while production remained `NoAuto`.

The new Gate G2 composition tests do not copy the source counts owned by the
I4-I10 inventories. They name each authority and require its exact test
declaration to remain at the reviewed source boundary; the ordinary suite then
executes both the authority and the gate latch. This preserves one source of
truth for each changing representation.

## Complete Production Family Accounting

Exactly four production semantic families allocate in the Glam-managed heap:

| Stable family | Exact trace and layout | Allocation, first owner, and reclamation |
| --- | --- | --- |
| `ManagedValueNode` | Wildcard-free `Value` dispatch; current x86-64 layout 64/8; requested extent 64 bytes. | Private `RuntimeValueAccess::root_managed_value` allocates and registers the intended root in one bounded region. Its family fixture proves allocator acceptance, rooted survival, and unrooted reclamation. |
| `ManagedLazyCell` | Stable source/result snapshot; current layout 144/8; requested extent 144 bytes. | Private raw allocation remains inside `RuntimeValueAccess`; construction publishes a containing value root or `ManagedLazyRoot`. The former-gap and family fixtures force the chronology and reclaim the unrooted cell. |
| `ManagedPromiseCell` | Exact successful assignment/failure edges; current layout 104/8; requested extent 104 bytes. | Private raw allocation and exact containing/root publication follow the same regional rule. Its family fixture proves acceptance, survival, and reclamation; publication mutations are root-authorized. |
| `ManagedCoreNetCell` | Quiescent logical payload walk under the net mutex; current layout 248/8; requested extent 248 bytes. | Private raw allocation publishes a containing value root or `ManagedCoreNetRoot`. Exact per-edit delta gateways own post-publication writes; the former-gap and family fixtures prove the first-owner boundary. |

The scalar `ManagedFamily for u64` is a private collector-access probe, not a
semantic production family. Compatibility and persistent/net managed nodes in
tests remain isolated trace/reclamation fixtures rather than production
allocation families.

All four production families use `managed_slot_extent<T>()`; the tests now
compare every `Trace::REQUESTED_SLOT_SIZE` with that policy directly. The
generic collector suite independently verifies requested total slot extent,
alignment rounding, canonical type metadata, repeated/concurrent class
discovery, typed-run header recovery, unsupported-layout rejection, and
cross-heap class rejection.

## Graph and External-Owner Closure

The final graph account has the following boundaries:

- every recursive `Value` route reaches an exact managed lazy, promise, or
  core-net identity through the wildcard-free compatibility visitor;
- persistent list and dictionary spines retain a reviewed non-forcing logical
  visitor, including deferred tails and shared/versioned shapes;
- core-net payloads and retained copy sources report exact managed edges, and
  every semantic edit uses an exact delta gateway under the same net mutex;
- caches, coordinator records, diagnostics/events, compiler state, reflection
  state, and public values retain registered roots under the durable-owner
  inventory rather than hiding interior `Gc` pointers;
- deferred host-call values remain explicit traceable captures; only the
  arbitrary Rust callback environment is a conservative external owner; and
- the four external-only opaque families are exhaustively mapped to edge-free
  provenance/tokens or reviewed lifecycle capabilities. No managed opaque arm
  or scoped managed downcast exists.

I10D's aggregate closure/opaque/`Any` latch and the independent managed-
backedge exclusion both pass. The active-owner graph remains external to the
managed graph, and no managed payload strongly retains the runtime value
domain.

## Unsafe, Mutation, and Region Audit

The production integration-side unsafe surface is narrow and accounted for:

- `Trace` plus private `ManagedFamily` implementations for the four managed
  families;
- the exact same-heap traced-edge borrow used only beneath
  `RuntimeValueAccess`;
- owner-qualified structural edge transitions for lazy source/result,
  promise assignment, and synchronized core-net edits;
- private opaque-family admission and owning downcasts through the matching
  runtime's external-owner registry; and
- the collector's independently audited typed-pointer, trace-dispatch, root,
  allocation, sweep, and finalization boundaries.

The I3 inventories still require bounded mutator regions, mutator-free effect,
event, diagnostic, and host callbacks, post-region reflection activation,
heap-qualified worker-local caches, and worker cache retirement. Core-net
tracing uses `try_lock` only after runtime quiescence gives the collector
exclusive graph authority; it neither reduces nor materializes the net.

## Audit Findings

### G2-001 — Recursive managed-family stable rows omitted explicit layout admission

**Classification:** documentation and executable-latch gap  
**Gate status:** resolved

The layout baseline named the natural sizes of `ManagedLazyCell`,
`ManagedPromiseCell`, and `ManagedCoreNetCell`, and their family lifecycle test
already allocated and reclaimed them. Unlike the `ManagedValueNode` stable
row, however, their individual ledger rows did not explicitly state requested
slot extent, allocator acceptance, private raw allocator, and exact first
owner.

The three rows now contain that evidence. `recursive_cell_layouts_are_recorded`
also directly compares each requested extent with `managed_slot_extent<T>()`,
and `gate_g2_stable_ledger_records_are_complete` prevents any of those fields
from silently disappearing.

### G2-002 — No independent final gate composed the authoritative inventories

**Classification:** planned certification gap  
**Gate status:** resolved

I10D closed the last component inventories, but correctly did not certify the
larger gate. `gate_g2_source_inventory_is_closed` now maps every Gate G2
concern to its existing authoritative verification, requires the dated review,
and latches the unchanged `NoAuto` runtime policy. It introduces no competing
source-count baseline.

### G2-003 — The recursive-cell escape latch initially classified the gate inventory as production

**Classification:** verification integration gap  
**Gate status:** resolved

The first complete-suite run correctly rejected the new test-only gate module
because it names private recursive cell types while reconciling their ledger
rows. The escape latch now excludes that module alongside the existing
test-only recursive-identity and active-owner inventories. The gate module can
only read source and documentation; it neither constructs nor exposes a
managed cell. The exact escape test and complete suite pass after that
classification.

No implementation defect, untraced recursive edge, unsafe-boundary defect, or
unclassified production owner was found.

## Verification

The certification ran:

- both Gate G2 composition tests;
- every I9/I10 final aggregate named by I11A;
- the managed value-node and three recursive-cell family/layout/lifecycle
  fixtures;
- every isolated compatibility, persistent collection, and core-net cycle
  reclamation fixture in the ordinary suite;
- the complete `glam-gc` library suite, including layout/class and
  finalization coverage; and
- formatting, warnings-denied Clippy, and the complete repository suite.

Verification passed on 2026-09-11: `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`,
`cargo test -q -p glam-gc --lib` (193 passed, 2 ignored), and `cargo test -q`
(1,450 main library tests plus every auxiliary target).

## Gate Decision

Gate G2 passes on 2026-09-11. The production ownership graph, its conservative
external boundaries, all four managed family records, their first-owner
chronology, and their isolated reclamation evidence are closed. I11B may now
introduce controlled forced collections over the complete production runtime.

This decision does not change any live heap's immutable policy, expose a
public collection API, or authorize automatic/maintenance collection.
Production remains `NoAuto` through the later I11 and I12 gates.
