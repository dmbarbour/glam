# Glam GC Integration Phase I9 Review — 2026-09-11

Baseline: `b72fec1`, the completed I9A-G implementation. The reviewed changes
are contained in that commit; documentation reconciliation performed by this
review follows it.

Status: complete. No open finding invalidates the runtime-root lifecycle,
external active-RAII, or source-inventory boundary, and no finding blocks
I10. Production remains `CollectionPolicy::NoAuto`; this review does not
authorize collection over a complete production runtime.

## Scope and Method

This review compared I9 with the integration plan, ownership ledger, I4F
durable-owner baseline, I5 recursive-identity and active-owner inventories,
I6/I7 closeout, and I8 review. It audited:

- every I5-I8 change to a durable field, owner, publication path, or retirement
  path against the I4F owner which already enclosed it;
- the runtime/cache, coordinator/evaluation, reflection, diagnostic/event, and
  compiler/assembly/CLI subsystem rows required by I9A-E;
- every production `Drop` implementation with active behavior, including
  active owners without a value root;
- direct and transitive exclusion of active external owners from managed
  allocations;
- temporary root ownership and retirement for I8's root-only core-net holders;
- the complete current source inventory of values, roots, failures, nets,
  callbacks, and type-erased attachments; and
- the assumptions with which I10-I13 consume the I9 boundary.

Source-backed inventories are used as change detectors. Existing forced-order
tests remain the lifecycle oracle; repetition is not treated as evidence for a
concurrent ordering.

## Plan-to-Implementation Accounting

| Checkpoint | Implemented disposition and evidence |
| --- | --- |
| I9A-E | `LIFECYCLE_DELTA` records I5 recursive identities/coordinator roots, I6 reflection ownership, I7's unchanged persistent representation, I8 root-only net holders, and every required unchanged subsystem. Each changed row names existing behavior, owner-drop, and isolated reclamation tests, and source-latches those tests. `runtime_root_lifecycle_delta_is_reconciled` rejects an omitted phase, subsystem, or verification. |
| I9F | `ACTIVE_RAII_INVENTORY` is a syntax-backed exhaustive inventory of all 19 production active `Drop` implementations. It distinguishes external lifecycle owners, detached retirement work, bounded claim/admission guards, the edge-free net notification companion, and runtime infrastructure. `EXTERNAL_OWNER_FAMILIES` records capability, root, explicit retirement, fallback, terminal, lock, and verification contracts for the five semantic lifecycle families. `managed_graph_reaches_no_active_raii_owner` composes the direct managed-cell exclusion with the existing exhaustive value/frontier and containment inventories. |
| I9G | `runtime_root_source_inventory_is_reconciled` reuses the I4F syntax-backed declaration scan rather than creating a competing baseline, then requires the I5 recursive-identity, I9F active-owner, and public/runtime root-publication inventories. No declaration is unmatched. |

## Ownership and Lifecycle Review

I9 introduces no production owner, root, managed allocation, callback, or
destructor. I5's managed lazy, promise, and core-net cells remain exact
interior identities under the root surfaces established in I4F. I6's
reflection computation directly traces effect and target values; its stable
observation is edge-free and its one-use activation permit is the only
post-I5 active-owner delta. I7 changed no owner representation.

I8 changed three existing temporary net owners. `CorePreparedCopySource`,
`CoreFrontierObservation`, and `NormalizationRequest` each retain one
root-only `ManagedCoreNetRoot` and derive an edge-only `CoreRuntimeNet` only
inside matching access. The prepared-copy holder already had a collection
fixture. I9 adds equivalent isolated retain-then-drop fixtures for frontier
observations and normalization requests. All three now prove exactly one
registered root while live and exact reclamation after descriptor retirement.

The active-RAII scan is deliberately broader than a search for `Root` fields.
A `Drop` implementation which closes a net disturbance, restores a claim,
releases mutation admission, retires a query, or shuts down workers can affect
runtime behavior without containing a value root. Requiring every production
implementation to have an explicit disposition prevents such a destructor
from silently entering a managed type later. This does not imply all 19 types
are semantic lifecycle owners: bounded guards and infrastructure remain
separate from the five external owner families.

`managed_graph_reaches_no_active_raii_owner` is not presented as a standalone
transitive Rust type proof. It checks the four production managed cell
declarations directly, while the compile-exhaustive `Value` walk, recursive
identity inventory, opaque/closure containment inventory, and three extracted
active-destruction frontiers close the transitive paths. The combined evidence
retains I4.0's rule that managed destruction is passive.

## Drift Classification and Resolution

### Intentional and justified

- I9 is a delta audit rather than a second owner conversion. Unchanged I4F
  owner rows reuse their existing collection matrix.
- One exhaustive active-`Drop` inventory covers non-rooting guards and
  infrastructure as well as external owners. Their differing dispositions are
  retained rather than flattening them into one lifecycle category.
- I9G calls the I4F declaration scan directly. Maintaining a second count and
  fingerprint would make two supposedly authoritative source inventories able
  to drift independently.
- No new concurrency tests duplicate the forced promise, session,
  client-demand, reflection activation, claim-unwind, and unlock-before-wake
  schedules already established by their source phases.

### Corrective information recorded

- I8 had structural inventories for `CoreFrontierObservation` and
  `NormalizationRequest`, but no focused owner-drop collection fixture. I9
  supplied both.
- The first frontier fixture opened an additional test-local managed-access
  gateway. The existing regional-entry source latch rejected it. The final
  fixture obtains the root through the already-reviewed prepared-copy gateway,
  so the test adds evidence without enlarging the access surface.
- The ownership ledger still described the root facade as I9 work and omitted
  the final temporary-net-root lifetime evidence. Those descriptions now
  distinguish I4 installation from I9 audit, and `src/README.md` records the
  inventory modules' current role.

### Accidental drift

No unresolved accidental implementation drift was found.

## Future-Phase Review

- **I10A remains correctly scoped.** I9 classifies destructor/lifecycle
  behavior, but arbitrary external callback captures remain opaque to Rust
  tracing and still require the planned containment audit.
- **I10B.0 remains a required design gate.** I9 does not decide whether opaque
  storage remains external-only or gains a sealed managed arm. Its active-RAII
  and source inventories are now stable inputs to that decision.
- **I11 Gate G2 can consume I9 directly.** Its source closure should require
  `runtime_root_lifecycle_delta_is_reconciled`,
  `active_external_raii_inventory_is_reconciled`,
  `managed_graph_reaches_no_active_raii_owner`, and
  `runtime_root_source_inventory_is_reconciled` rather than rebuilding the
  same ledgers.
- **I12 is unaffected.** I9 changes no collection policy, readiness activity,
  or runtime maintenance authority.
- **I13 should retain only one authoritative source inventory per concern.**
  Cleanup may consolidate implementation location, but must not weaken the
  durable-owner, active-`Drop`, or recursive-identity change detectors before
  the remaining compatibility walk is retired.

## Verification

The reviewed boundary is covered by:

- the four named I9 reconciliation tests;
- all 17 existing `inventory_is_complete` tests;
- three isolated temporary-net-owner lifetime fixtures;
- existing promise resolver/producer, session, client-demand, reflection,
  query, subscription, claim-unwind, net-close, executor, and mutation-gate
  lifecycle tests named by the active-RAII inventory;
- the complete recursive-identity cycle matrix; and
- the routine formatting, lint, and repository test suite.

The implementation checks passed on 2026-09-11: `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test -q`
(1,425 library tests plus every auxiliary target). No unsafe boundary or
production collection path was added.
