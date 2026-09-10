# Glam GC Integration Phases I6-I7 Review — 2026-09-10

Baseline: `3f46bf9`, containing the completed I6 immutable-shell audit and I7
persistent-container delta audit.

Status: complete. No open finding blocks I8. Production remains
`CollectionPolicy::NoAuto`; this review authorizes neither production-graph
collection nor a new managed allocation family.

## Scope and Method

I6 and I7 were deliberately grouped as one adjacent audit-only major stage
before implementation. The review compared the implementation with the GC
integration plan, collector plan, roadmap, ownership ledger, post-I5 review,
and current source representations. It checked:

- concrete representation, identity, trace, destruction, and durable-owner
  disposition for every I6 shell;
- the exact managed stop edge reached by each compatibility path;
- list, dictionary, key, thunk, slice, concatenation, and finger-tree coverage;
- rooted survival and exact unrooted reclamation for newly added topology;
- source-backed visitor, managed-entry, and durable-owner latches;
- production behavior tests for metadata, failures, fixpoints, applications,
  net construction, and persistent collections; and
- I8-I13 entry assumptions after retaining every audit-only representation.

The implementation adds only tests, a shared test helper, and inventory or
documentation reconciliation. It changes no production representation,
semantic identity, mutation gateway, destructor, callback boundary, or
collection policy.

## Plan-to-Implementation Accounting

| Checkpoint | Implemented disposition and evidence |
| --- | --- |
| I6A.0 | `NetValue`, `FunctionCode`, and `FunctionValue` remain immutable scalar shells over one exact managed net identity. `core_operator_adapter_enumerates_every_value_and_net_payload` proves identical projection without reduction; shared-stage and managed function-stage tests preserve identity/cycle behavior. |
| I6A.1 | `BuiltinCall` and `LazyApplication` retain exact ordered compatibility visitors. The existing partial-builtin cycle and new `managed_cycle_through_lazy_application_is_traced_and_reclaimed` fixture close both paths. |
| I6A.2 | Both `FixpointComputation` variants remain one-value immutable shells. Direct visitor assertions now cover both variants, and function/object backedge fixtures reclaim exactly. |
| I6B.1-B.2 | `MetadataCarrier` remains one identity-bearing `Arc<Value>` with one exact edge. Existing pure/reflection update, inspection, `seq`/`spark`, identity, and metadata-cycle suites pass. No external semantic owner or independent conversion benefit was found, so I6B.2 is not required. |
| I6C.1 | `EvaluationFailure` retains emission-or-cycle data and ordered contexts. Exact visitation, the no-semantic-service sentinel, and both emission/context reclamation fixtures pass. |
| I6C.2-C.3 | `RuntimeFailureRoot` remains an external shallow registered-root owner. `runtime_failure_root_alone_retains_and_releases_its_managed_values` proves its direct graph lives only for that owner. Managing the immutable failure shell would not eliminate this deliberate report lifetime, so I6C.3 is not required. |
| I6D.1 | Consumed unchanged from closed GCI5R-005: reflection effect/target are managed-reachable, while reservation observation is edge-free and activation ownership is one-use. |
| I6D.2 | Net construction remains one immutable effect edge below its managed lazy identity. Direct visitation, blocked polling, transition tests, and `managed_cycle_through_net_construction_is_traced_and_reclaimed` close the path. |
| I7A | Git/source inspection found no post-I4D persistent representation change. `persistent_representation_to_visitor_inventory_is_complete` covers every current list/chunk shape, strict sub-slices, both thunk variants, map versions, and recursive key forms through exhaustive matches. |
| I7B | `managed_promise_cycle_through_list_thunk_is_traced_and_reclaimed` closes the sole missing production topology. No I6 checkpoint introduced another managed payload shape. |
| I7C | Logical counters now assert sub-slice and versioned-map work in addition to the existing empty, byte, strict-value, concat, finger-tree, thunk, nested-key, and duplicated-spine cases. Physical deduplication remains profiling-driven future work. |

## Ownership, Trace, and Destruction Review

The retained I6/I7 shells contain ordinary immutable Rust ownership only.
Their compatibility visitors recurse synchronously until encountering
`ManagedLazyEdge`, `ManagedPromiseEdge`, or `ManagedCoreNetEdge`; the managed
identity visitor then stops recursion and reports that exact edge. None of the
audited shells creates a second mutable identity.

All transitive destruction remains passive under the I4.0/I4F closure gate.
The only I6 external value owner is the already-established
`RuntimeFailureRoot`: it is not representable inside `Value`, contains shallow
registered roots rather than interior `Gc` edges, and retires with its
coordinator/report owner. Metadata pointer identity remains internal and
cannot be compared by Glam evaluation.

Persistent structures remain immutable RPDS/FingerTree/`Arc` spines. Their
logical visitors do not force thunks or mutate collection state. Repeated
shared-spine visits may cost work but cannot omit an edge; collector marking
deduplicates the eventual managed identity. No mutation barrier is needed for
an immutable published spine.

## Drift Classification

### Intentional and justified

- I6A-C and I6D.2 completed as audits rather than representation migrations.
  Each possible conversion would add allocation/indirection without removing
  an external backedge, mutable identity, or ownership boundary.
- I6 and I7 used one grouped review because neither was authorized to create a
  managed family and both consume the same exact compatibility walk.
- The lazy/promise cycle setup was factored into one test helper so each shell
  topology differs only in the source constructor under examination.

### Corrective evidence added

- Object-fixpoint, lazy-application, net-construction, and deferred list-tail
  backedges were absent from the isolated production reclamation matrix.
- Durable failure roots had direct occurrence/root tests but no isolated proof
  that the wrapper alone retains and then releases the complete managed graph.
- The persistent representation fixture did not explicitly identify itself as
  the exhaustive inventory or assert a strict shared sub-slice and versioned-
  map accounting result.

All five gaps now have deterministic, isolated fixtures. The added
failure-root fixture also updated the source-backed managed-entry count; the
full inventory caught that change before closeout.

### Accidental drift

No unresolved accidental implementation drift was found.

## Future-Phase Review

- **I8A.0 remains necessary.** `CoreRuntimeNet` still pairs its exact managed
  edge with weak value-domain provenance, unlike the simplified lazy/promise
  facades. Removing that observer remains a coordinated net-wide change.
- **I8A.1-I8A.3 remain correctly partitioned.** I6/I7 add no payload
  representation or mutation gateway, so payload reconciliation can consume
  their retained visitors unchanged before exact net edit deltas and lifecycle
  revalidation.
- **I8B has no new I6/I7 managed-family matrix.** It should add only topology
  states not covered by I5 plus changes introduced by I8 itself. The existing
  I6/I7 fixtures remain inputs rather than tests to duplicate.
- **I9 begins with no unrecorded I6/I7 durable-owner delta.** I6D.1's
  reflection owner change is already source-latched and reviewed; I8 changes
  will determine any additional I9 subsystem work.
- **I10-I13 remain coherent.** Host callbacks and opaque storage are still the
  open containment boundaries. Gate G2 still requires temporal publication
  evidence for any later managed family, and I13 remains cleanup rather than a
  delayed correctness phase.

## Verification

Passed on 2026-09-10:

- all focused I6/I7 visitor, behavior, lifecycle, and reclamation fixtures;
- `cargo fmt --check`;
- `cargo clippy --all-targets --all-features -- -D warnings`;
- `cargo test -q`; and
- `cargo test --workspace -q`, including 194 passing `glam-gc` library tests
  with two intentionally ignored tests.

No new unsafe boundary or schedule-dependent production behavior was added,
so this audit-only stage does not add a Miri or forced-order requirement.
