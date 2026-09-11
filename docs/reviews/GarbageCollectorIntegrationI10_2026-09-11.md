# Glam GC Integration Phase I10 Review — 2026-09-11

Baseline: `c0015f1`, the completed I10A-I10C implementation. I10D's final
certification tests and the documentation reconciliation performed by this
review follow that baseline.

Status: complete. No open finding invalidates deferred-callback containment,
the external-only opaque model, external-owner retirement, or managed
backedge exclusion. No finding blocks I11A. Production remains
`CollectionPolicy::NoAuto`; this review does not authorize collection over a
complete production runtime or itself certify Gate G2.

## Scope and Method

This review compared I10 with the integration plan, ownership ledger, managed
family and durable-owner inventories, post-I8 and post-I9 reviews, and the
current implementation. It audited:

- every production deferred Rust callback and the separation between its
  traceable Glam captures and opaque callback environment;
- every production `OpaqueValue` family, constructor, admission, downcast,
  and type-erased cache family;
- direct and transitive exclusion of strong runtime/value-domain authority,
  registered roots, and active external owners from managed payloads;
- matching-runtime access and shared external-owner identity;
- effect-token, task-handle, query, and general external-owner retirement;
- managed destruction, registry lock/drop ordering, panic recovery, domain
  teardown, and conservative live-root retention;
- the final closure, `Any`, cache, durable-root, recursive-identity, and
  managed-backedge source reconciliation required by I10D; and
- the assumptions with which I11-I13 consume the closed I10 boundary.

Source-backed inventories are change detectors, not substitutes for semantic
tests. The final aggregate calls or source-latches existing independently
authoritative inventories instead of establishing a competing source-count
baseline.

## Plan-to-Implementation Accounting

| Checkpoint | Implemented disposition and evidence |
| --- | --- |
| I10A | `HostCallProducer` stores recursive Glam captures explicitly and traces them through its managed lazy source. Immediately before the opaque callback, those captures become one typed same-runtime `HostCallRootBundle`; the callback runs after managed access closes. Production compiler loaders retain weak assembler/compilation routes. The exhaustive callback inventory assigns every remaining closure boundary a concrete external, bounded, explicit-field, traceable-value, or notification disposition. |
| I10B.0 | The bootstrap selected one external-only opaque representation. A managed value stores only a passive `ExternalOwnerHandle`; arbitrary Rust payloads live in the value domain's external-owner registry. No managed opaque arm or scoped managed downcast is authorized. |
| I10B.1 | Four compile-exhaustive production opaque families are recorded: edge-free `CompilationOrigin` and `ConstructionPort`, plus external-capability `EffectToken<T>` and `TaskHandleCell`. Constructor, admission, downcast, `Any`, and runtime-cache inventories reject an unrecorded family. |
| I10B.2 | The private `OpaquePayloadFamily` admission contract requires a source-backed ownership record. Compile-negative fixtures reject a bare managed pointer, an unrooted core value, and a foreign runtime root; direct field-shape tests latch every admitted family. |
| I10B.3 | Effect-token domains and task/query handles remain explicit external root owners with idempotent retirement. Their conservative retention can delay collection but cannot prematurely reclaim a live value or hide a managed node's own recursive edge. |
| I10B.4 | Owning opaque access requires the matching runtime and preserves one shared registry-owner identity across cloned values. Reflection caps remain the only public route to compilation-origin observation; construction ports remain invocation-local. |
| I10C.1 | Dropping a managed opaque shell releases only its passive scalar handle. Retired external owners are selected under the registry mutex and destroyed one at a time after unlock. |
| I10C.2 | Token, task-handle, and query lifecycles use their existing explicit retirement paths. Repeated retirement is harmless, and terminal handles release their final query leases without making managed destruction active. |
| I10C.3 | Domain teardown passively destroys remaining external owners. A panicking owner is terminally removed while the untouched suffix remains registered for a later drain; conservative external roots remain live until explicit retirement or domain teardown. |
| I10D | `final_closure_opaque_and_any_inventory_is_reconciled` executes the four local closure/opaque source gates and latches the cache, durable-root, recursive-identity, and active-owner gates. `managed_payloads_have_no_strong_value_domain_backedge` directly checks every managed allocation or managed-reachable callback/opaque frontier for forbidden strong authority and composes that result with the exhaustive transitive inventories. |

## Ownership, Containment, and Destruction Review

I10 introduces no second semantic identity. Managed lazy state reaches
`HostCallProducer`, whose `captures: Arc<[Value]>` remains part of the exact
managed trace. Its erased callback is held outside the managed graph under an
`ExternalOwnerHandle`. Invocation temporarily roots only the declared
captures, ends the access region, and transfers those roots into the callback.
The callback environment may independently retain public roots supplied by
embedding Rust, but that is an honest external ownership extension rather than
an unreported managed backedge.

`OpaqueValue` follows the same separation. Its managed representation is a
passive handle, while the value domain's external registry owns the admitted
`Arc<T>`. The two edge-free families contain only provenance or construction
identity. The two capability families can reach external runtime state and
registered roots, so their lifecycle is accounted as external active RAII and
cannot be used as a companion-state classification. No production payload
contains `Gc<T>`, a raw recursive core value, or `RuntimeValueRoot` directly.

External-owner retirement does not run destructors under the registry lock.
One selected entry is removed and then dropped; if that drop panics, it cannot
be retried, while entries not yet selected remain registered. Domain teardown
uses the same passive managed boundary and external destruction model.
Collection itself does not retire an external owner merely because only an
opaque handle remains, so accepted conservative roots remain sound at the cost
of potential delayed reclamation.

The final backedge latch is deliberately compositional. A spelling scan of the
seven managed or managed-reachable declarations catches a newly embedded
runtime service or root immediately, while the wildcard-free value visitor,
recursive-identity inventory, opaque-family inventory, callback inventory,
durable-root inventory, and active-owner graph exclusion establish the
transitive classification. Neither check is presented as sufficient alone.

## Drift Classification and Resolution

### Intentional and justified

- Arbitrary Rust callback environments and opaque capabilities remain
  conservative external owners. Requiring them to implement a trace protocol
  would make the embedding boundary unsoundly claim knowledge of erased Rust
  state.
- `OpaqueValue` has no managed payload arm. The external-only representation
  is sufficient for the four bootstrap families and keeps managed destruction
  passive; a future managed opaque family requires a new design review.
- I10D composes the authoritative inventories from their owning modules rather
  than copying all source counts into a second final table.
- A panic while dropping one retired external owner removes that attempted
  owner but preserves the untouched suffix. Retrying a destructor after it
  unwinds would not be a sound lifecycle guarantee.
- Conservative external ownership may retain values beyond their last
  semantically useful opaque handle. That is accepted bootstrap retention, not
  a collector-visible cycle or premature-reclamation defect.
- External-owner IDs order candidates within one drain, but concurrent drains
  may interleave destructors. Exactly-once detachment is guaranteed; destructor
  order is deliberately not language semantics.

### Corrective information recorded

- The planned I10D certification tests had not been implemented after I10C;
  this checkpoint adds both named gates before the review is closed.
- The integration plan and ownership ledger still described I10 as pending,
  the roadmap still described I5 as blocked, and architecture notes still
  described the pre-I4 root representation. They now record the managed
  inline-or-root facade and completed I10 boundary.
- The ownership ledger's x86-64 `RuntimeValueRoot` measurement was the old
  72-byte compatibility shape. The current private inline-or-managed-root
  representation measures 32 bytes and the ledger now records that value.
- I11C said collection would finalize passive opaque payloads. Active opaque
  payloads are external registry owners; collection finalizes only the passive
  managed value shells, and external retirement remains a separate operation.
- Registry comments described ID sorting as a deterministic retirement order.
  They now distinguish deterministic per-drain selection from intentionally
  unordered concurrent destruction.

### Accidental drift

No unresolved accidental implementation drift was found.

## Future-Phase Review

- **I11A remains mandatory.** It must consume both I10D aggregate gates,
  independently rerun the complete source and collector layout/class evidence,
  and produce the dated Gate G2 certification. This review closes I10; it does
  not enable production collection.
- **I11B must distinguish ownership outcomes.** Managed cycles must reclaim;
  conservative external callback and opaque owners must retain or retire
  according to their recorded external lifecycle rather than being mistaken
  for missing trace edges.
- **I11C finalizes managed shells, not active opaque payloads.** External-owner
  retirement and destruction continue after collector locks and managed
  finalization have ended.
- **I12 remains unaffected.** Production stays `NoAuto`; readiness accounting,
  explicit maintenance, and any automatic policy remain later decisions.
- **I13 may consolidate inventories only after Gate G3.** It must preserve one
  authoritative change detector for callbacks, opaque/type-erased families,
  durable roots, recursive identities, and active external owners until the
  remaining compatibility value walk is replaced.

## Verification

The reviewed boundary is covered by:

- I10D's two final aggregate certification tests;
- the complete closure/callback, opaque-family, `Any`, cache, durable-root,
  recursive-identity, and active-owner source inventories;
- compile-negative opaque admission fixtures;
- matching-runtime access, owner identity, and reflection-cap tests;
- managed deferred-cycle reclamation and declared-capture retention fixtures;
- external token/task/query retirement, unlock-before-drop, panic recovery,
  domain teardown, and conservative live-root tests;
- the focused `glam-gc` finalization suite; and
- the routine formatting, lint, and repository test suite.

The post-review verification passed on 2026-09-11: `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`,
`cargo test -q -p glam-gc --lib` (193 passed, 2 ignored), and `cargo test -q`
(1,448 main library tests plus every auxiliary target). No unsafe boundary or
production collection path was added by I10D or this review.
