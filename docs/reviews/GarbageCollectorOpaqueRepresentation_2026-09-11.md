# Glam GC Opaque Representation Decision Review — 2026-09-11

Baseline: `7f3cf24`, the completed I10A deferred-closure containment
checkpoint.

Status: complete. Phase I10B.0 selects **external-only opaque storage** for the
bootstrap. `OpaqueValue` remains a passive handle into the runtime's external-
owner registry. No managed opaque arm, managed opaque allocator, scoped
managed downcast, or opaque managed-family finalizer is authorized.

## Scope and Method

The review consumed the I4B constructor/admission inventory, I4F durable-owner
inventory, I9 external active-RAII inventory, and I10A callback inventory. It
then inspected every production `OpaquePayloadFamily`, `OpaqueValue::new`
constructor, typed downcast, type-erased owner, and runtime-cache family.

The deciding question was not whether a payload can be represented in managed
memory. It was whether an actual bootstrap payload needs a collector-visible
edge badly enough to justify a second sealed representation, mutator-bound
access, a new allocation family, and separation of passive managed destruction
from any active retirement behavior. None does.

## Production Opaque Family Inventory

| Family and source | Stored representation and access | Selected policy and retention cost |
| --- | --- | --- |
| `CompilationOrigin`; `src/diagnostic.rs` | An immutable `CompilationTrace` containing source identity, digest, namespace labels, and parent provenance. One constructor and one matching-runtime downcast produce ordinary diagnostic data. | External edge-free provenance. It has no runtime capability, root, `Value`, or managed pointer. Registry retirement may delay release of Rust provenance only. |
| `ConstructionPort`; `src/eval/builtins/net/construction.rs` | One construction-local `Arc<ConstructionBrand>` and scalar port ID. One production constructor family and one matching-runtime downcast additionally compare the brand. | External edge-free token. It has no semantic edge or active destruction. Registry retirement may delay release of a construction brand only. |
| `EffectToken<T>`; `src/api/value.rs` | A scalar token ID and weak route to a host-owned `EffectTokenDomainState<T>`. The generic payload remains in the external domain map. `resolve` downcasts the token, checks the exact domain, and returns an owning `Arc<T>` to the host. | External lifecycle capability. Token `Drop` removes the domain entry if the external domain still exists. A domain may intentionally own public values, but the managed token has only a weak route back to it. The domain's explicit host lifetime bounds any rooted retention. |
| `TaskHandleCell`; `src/reflection/requests.rs` | Runtime/task identity, an `EvaluationTaskHandle`, and an `Arc<EvaluationQueryHandle>`. One wrapper constructor and one matching-runtime downcast support task status/value/error/join/cancel operations. | External lifecycle capability. Query retirement and task observation remain active external behavior, so this payload cannot become an ordinary managed finalizer. Its task/query state may retain rooted terminal data. A pathological result-to-own-handle cycle can therefore remain conservatively retained until runtime teardown; the bootstrap accepts that cost. |

The two edge-free families need no managed trace. The two lifecycle families
need active external coordination rather than managed finalization. Moving
only their identity shells into a sealed managed arm would not make the
external task/domain state traceable and would add a second representation
without closing the significant lifecycle boundary.

## Other Type-Erased Boundaries

`ExternalOwnerRegistry` is the common external `Box<dyn Any + Send + Sync>`
owner for opaque payloads and deferred host callbacks. It stores a `TypeId`, a
weak lease, and the owning `Arc<T>`; managed values retain only a runtime-local
handle and ordinary lease. Retirement detaches entries under the registry
mutex and destroys them after unlocking.

`RuntimeValueCache` is separate from `OpaqueValue`. Its admitted
`RuntimeCacheFamily` entries enumerate every retained `RuntimeValueRoot` before
publication. The two production compiler cache families remain runtime-owned
root surfaces and are not candidates for a managed opaque arm.

The remaining `dyn Any` occurrence only inspects a caught Rust panic payload
for a string. It owns no persistent payload and has no GC relationship.

## Access and Identity Contract

The selected external representation preserves the current contract:

- construction is private and requires the unsafe, source-recorded
  `OpaquePayloadFamily` admission;
- `OpaqueValue` stores only one `ExternalOwnerHandle`;
- downcast requires a matching `CoreValueFactory`, rejects another runtime or
  family, and returns an owning `Arc<T>` after the registry lock is released;
- all four production consumers rely on that owning access shape, while no
  consumer needs a mutator-bound borrow;
- Glam equality compares opaque owner identity through the shared lease, not
  payload equality; and
- arbitrary `Any`, bare `Gc<T>`, raw recursive `Value`, `RuntimeValueRoot`, and
  foreign-runtime roots remain forbidden inside an opaque payload.

The owning `Arc<T>` downcast is appropriate precisely because these are
external owners. It would be wrong for a future sealed managed arm, but that
arm is not selected here.

## Rejected Bootstrap Alternative

A sealed managed arm would require a private representation beside `Any`, one
registered allocation family per admitted type, exact tracing and passive-drop
records, matching-runtime provenance, and mutator-bound access. None of the
current edge-free families benefits. The task handle would additionally need
a lifecycle redesign which separates managed identity from query/task
retirement; merely tracing its current Rust fields would violate the passive
managed-destruction boundary. That work is disproportionate to the accepted
conservative retention of an explicit reflection capability.

A future payload reopens this decision only when it has a concrete recursive
managed edge whose conservative external retention is materially harmful and
when its destruction can satisfy I4.0. Such work requires a new design review;
it is not an extension of `OpaquePayloadFamily`.

## Plan Resolution

- I10B is partitioned into the source inventory, edge-free-family closure,
  external-capability lifecycle closure, and access/negative-boundary closure.
- I10C contains only external lifecycle and passive-handle destruction audits;
  it introduces no managed opaque access or finalizer.
- The ownership ledger, GC roadmap invariant, Gate G2, and integration
  completion criteria now state the selected external-only policy directly.
- Production remains `CollectionPolicy::NoAuto`; this review authorizes no
  collection path and introduces no managed allocation.

## Verification

The review artifact is latched by
`opaque_representation_review_inventory_is_complete`,
`opaque_representation_plan_has_no_undecided_family`, and
`opaque_representation_plan_links_are_consistent`. I10B will add the concrete
family/access/lifecycle fixtures named by the revised implementation phases.

