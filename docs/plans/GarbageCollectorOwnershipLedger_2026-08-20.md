# Glam GC Ownership and Mutation Ledger — 2026-08-20

Status: Phases I0 through I4 are complete and reviewed. The production
inline-or-registered-root facade, managed outer value node, passive active-owner
split, exact durable-owner inventory, and closed owner-level collection matrix
are current. Recursive payloads deliberately retain their compatibility Rust
ownership and I4 edge adapters until I5-I8 replace each family with exact
managed edges. Stable integration facts are reconciled when each later
representation family receives its concrete managed wrapper and trace
implementation.
Collector-private class topology is verified inside `glam-gc` and is not part
of this ledger. Every applicable family record must be complete before Gate G2
permits production collection.

This is the graph inventory required by
[`GarbageCollectorIntegration_2026-08-19.md`](GarbageCollectorIntegration_2026-08-19.md).
It records current ownership honestly; it does not imply that every Rust owner
will become a managed allocation. Missing or type-erased managed edges are
blocking defects rather than conservative roots.

## Classification

- **M** — intended managed graph node or a representation which must expose an
  exact trace adapter.
- **R** — ordinary Rust external owner which intentionally owns registered
  `RuntimeValueRoot` or public `Value` roots. It is not itself traced, and its
  semantic lifetime deliberately extends the rooted managed values.
- **C** — host-owned companion state associated with a managed node. It may
  contain locks, notifications, IDs, and other coordination data, but no
  `Gc`, `Root`, public `Value`, or equivalent hidden managed edge. It is not an
  external root owner and cannot keep the managed node or its graph alive.
- **T** — bounded compiler, parser, evaluator, transaction, or callback-local
  owner. Any direct managed borrow in it is enclosed by a mutator region;
  values retained between evaluator regions or across callbacks are exact
  same-runtime roots instead. The complete transient owner's lifetime need not
  equal one continuous mutator lifetime.
- **E** — external immutable leaf allocation with no managed edge.
- **D** — boundary defect which must be eliminated or converted to an exact
  same-runtime root before collection.

Mutation is **immutable**, **replaceable** under the named lock, **one-write**,
or **free** under the named exclusive guard. “Mutation gateway” names the
integration phase which routes a replaceable managed edge through the initial
collector's structural no-op gateway. This records the relevant write sites
without installing generational or concurrent-collector machinery.

Do not use an **R** record to avoid tracing an internal edge. In particular, a
promise assignment, lazy source/result, or fixpoint backedge held through a
registered root would hide the cycle this collector is intended to reclaim.
Such edges belong in **M** nodes. A public resolver, diagnostic subscriber, or
embedding-client handle may be **R** only when it is a genuine owner outside
the managed graph. The term *sidecar* is avoided below because it previously
blurred this distinction between an edge-free **C** companion and an **R**
external root owner.

The stable identity of a ledger row is its named Glam representation family,
concrete Rust type, and source owner. `TypeId`, canonical `ObjectMetadata`
addresses, and heap-local dense class IDs are implementation mechanisms rather
than durable documentation identities. Metadata addresses vary by process and
class IDs vary by heap discovery order; neither appears in a production
integration record.

When a family becomes managed, its reconciliation record contains:

1. the stable family name, concrete Rust type, and defining source path;
2. the trace-review checkpoint and exact outgoing-edge enumeration policy;
3. Rust size/alignment and the requested total slot extent selected by Glam;
4. whether allocator discovery accepts that requested layout;
5. whether the payload requires `Drop`, proof that its direct and transitive
   destruction is passive, any external explicit-retirement owner, and the
   relevant failure test;
6. every post-publication mutation gateway, or an explicit immutable policy;
7. its external-root classification and the source inventory proving that no
   internal edge was hidden behind a root; and
8. the migration phase and exact verification which authorizes collection.

Final aligned stride, slots per run, dense class identity, metadata address,
frontier state, and other derived run geometry remain collector-internal.
`glam-gc` layout/class tests verify them from public Rust layout and requested
extent inputs. A test-only diagnostic may report them for profiling, but Gate
G2 never depends on copying those instance-specific results into this ledger.
No family may enter even an isolated collection fixture while its stable
record, visitor, or direct/transitive destruction entry is unresolved. I4.0 is
the common admission contract; later family phases supply their concrete
evidence rather than deferring destructor review to I10.

## Current Layout Baseline

These are `size/align` measurements on the current x86-64 GNU Rust target.
They are diagnostics for representation planning, not a stable ABI.

| Rust type | Bytes/alignment | Projected role |
| --- | ---: | --- |
| `core::Value` | 64/8 | Inline tagged shell; visitor dispatch in I4. |
| `core::Key`, `Number` | 64/8, 64/8 | External/persistent leaves except nested key values, which are converted before storage. |
| `LazyValue`, `LazyCell`, `LazySource` | 8/8, 144/8, 40/8 | `LazyCell` becomes a typed managed node in I5. |
| `PromisedValue`, `PromiseCell` | 8/8, 192/8 | `PromiseCell` becomes a typed managed node in I5. |
| `MetadataCarrier`, `OpaqueValue` | 8/8, 16/8 | Metadata becomes managed in I6; opaque remains a checked boundary. |
| `NetValue`, `FunctionCode`, `FunctionValue` | 8/8, 24/8, 16/8 | Net identity integration in I5; remaining function shells in I6. |
| `BuiltinCall`, `EvaluationFailure` | 24/8, 80/8 | Exact visitors in I4/I6. |
| `List`, `ListThunk` | 8/8, 16/8 | Logical persistent-container trace in I7. |
| `RuntimeValueRoot`, `CoreOperator` | 72/8, 96/8 | Root facade in I2/I9; net payload trace/gateways in I5 and final audit in I8. |
| `ListNode`, `ListChunk`, `SharedSlice`, `FingerList` | 40/8, 40/8, 32/8, 16/8 | Existing external persistent spines, logically visited in I7. |
| `SharedRuntimeNet`, `SharedRuntimeNetInner`, `SharedRuntimeNetState` | 8/8, 256/8, 224/8 | Current synchronized owner/layout input. I5 replaces the production core owner atomically with lazy/promise identity; individual topology entries remain ordinary storage. |
| `RuntimeNet`, `RuntimeEntry`, `RuntimeNode` | 200/8, 120/8, 96/8 | Freely mutable only under the shared-net mutex. |
| `CopyState`, `ActivePairState` | 104/8, 48/8 | Net-internal mutable state and cross-net source edge. |

The collector supports per-type requested total slot extents before alignment
rounding, so these layouts do not impose a heap-wide size floor. A request is
not additional padding and cannot be smaller than the Rust representation.
Glam's eventual compact-value policy belongs to the value-representation plan.
Every future managed payload must fit one fixed-size typed-run slot; no current
row is approved for a large-object or multi-run exception.

I1D centralizes current Glam requests through
`core::managed::managed_slot_extent<T>`. Its initial pointer-sized minimum is a
conservative pre-representation baseline rather than a tagged-pointer or final
padding decision. Rust type alignment remains authoritative. Each production
managed family records the resulting requested extent and allocator acceptance
when introduced; the private I1D leaf probe is verification machinery and not
a semantic family row.

## I4 Production Managed Shell

The bootstrap GC integration uses one monolithic managed outer allocation for
the current `core::Value` variants. This is a correctness-first integration
shape, not the compact tagged representation selected by the later value-
representation refinement plan. `RuntimeValueRoot` stores small `i64` values
inline and stores every other value in this registered node. Recursive payloads
remain compatibility Rust ownership until I5 begins one transitive walk and
I5-I8 replace individual identity/structure steps, so the current production
visitor is exhaustively zero-edge rather than falsely tracing those payloads
as managed pointers.

| Stable family/type/source | Trace and layout | Destruction, ownership, and admission |
| --- | --- | --- |
| Production managed core value node; `ManagedValueNode`; `src/core/managed/value_node.rs` | Wildcard-free dispatch covers all thirteen `Value` variants and currently reports zero managed edges. Current x86-64 layout is 64/8; requested extent is 64 bytes and allocator discovery is exercised by rooted survival and unrooted reclamation. The layout is compile-time latched on that target. | Passive drop under I4.0: the node has no direct `Drop`; its compatibility `Value` payload was admitted by the I4F.2b closure gate after callbacks, reflection reservations, and opaque retirement moved to external owners. Cloned public/runtime handles share one registered root cell, and neither inline nor managed roots retain the value domain. I5 installs the transitive managed-edge walk and recursive identity leaves; I6-I8 replace or audit its remaining compatibility steps. |

The opaque arm above is variant-dispatch evidence only. It does not classify
`OpaqueValue` as an edge-free production leaf or weaken the I4B/Gate G2 opaque
blockers.

## `core::Value` Variant Ledger

| Variant | Current owner and outgoing edges | Mutation, threading, and longevity | Exact visitor / mutation gateway / migration |
| --- | --- | --- | --- |
| `Atom` | Interned `Key`; no `Value` edge. | Immutable, `Send + Sync`, long-lived. | Leaf; no mutation gateway; I4. |
| `Number` | Number-owned integer/rational storage; no `Value` edge. | Immutable, thread-safe, long-lived. | Leaf initially; I4. |
| `Binary` | `bytes::Bytes`; no managed edge. | Immutable shared leaf. | Leaf/external allocation; I4. |
| `List` | Persistent list nodes; value chunks and lazy/promise thunks are outgoing edges. | Immutable/persistent and thread-safe; may escape all evaluator calls. | I4D compatibility adapter reports strict values and thunks through a non-forcing logical walk, including repeated shared-spine occurrences; I5B composes it into the transitive managed-edge walk and I7 audits it. Bytes are leaves. No post-publication mutation. |
| `Dict` | `RedBlackTreeMapSync<Key, Value>`; every mapped value is an edge. Keys contain no live `Value` after conversion. | Immutable/persistent and thread-safe; may be runtime-global. | I4D compatibility adapter reports mapped values in key order and exhaustively classifies recursive keys as leaves; I5B composes it into the transitive managed-edge walk and I7 audits it. No post-publication mutation. |
| `Builtin` | Static enum only. | Immutable leaf. | Leaf; I4. |
| `PartialBuiltin` | `BuiltinCall.arguments: Arc<[Value]>`. | Immutable, shared across threads. | I4C compatibility visitor reports every supplied argument in order; I6 replaces it with the managed visitor. |
| `Function` | `FunctionValue -> NetValue -> CoreRuntimeNet`; the net carries data/operator values. | Immutable compatibility shell over the external synchronized core-net owner; long-lived. | I4E reports exactly one stage-net identity without inspecting it. I5 atomically migrates the net identity; I6 migrates the remaining function shell. |
| `Net` | `NetValue -> CoreRuntimeNet`. | Immutable compatibility shell over external synchronized runtime-net state. | I4E reports exactly one net identity, while the bounded core adapter enumerates its direct payloads without reduction. I5 installs the managed outer-cell trace and gateways; I8 performs the final audit. |
| `Lazy` | `Arc<LazyCell>`; source and terminal result graphs. | Identity-bearing, thread-safe, long-lived; source/result publication races are supported. | Managed cell visitor; one-write result plus replaceable source protocol; I5. |
| `Promised` | `Arc<PromiseCell>`; successful assignment root and producer obligation. | Identity-bearing, thread-safe, long-lived; assignment is one-write. | Managed cell visitor; assignment mutation gateway; I5. |
| `Metadata` | `MetadataCarrier.metadata: Arc<Value>`. | Immutable identity-bearing sealed value, thread-safe. | Visit exactly one metadata value; I6. |
| `Opaque` | `Arc<dyn Any + Send + Sync>`; payload-dependent. | Pointer-identity shell; arbitrary longevity/thread transfer. | I10B.0 is a hard review gate. Until it selects otherwise, opaque storage remains external: arbitrary `Any` is edge-free or owns audited same-runtime public roots. A possible managed arm must be a separate sealed exact representation outside `Any`, with one stable family record per admitted type. No managed opaque allocation or scoped managed downcast is authorized before the review. |

## Recursive Core Nodes

| Type and source | Outgoing edges | Mutation and synchronization | Drop / trace / mutation gateway / phase |
| --- | --- | --- | --- |
| `EvaluationFailure` (`core.rs`) | Emission `Value` or cycle IDs; `Arc<[Value]>` contexts. | Immutable, thread-safe, retained in task/report ledgers. | I4C reports an emission followed by ordered contexts; dependency-cycle IDs are non-value leaves. Ordinary Rust drop initially; I6 installs the managed visitor. |
| `LazyCell` / `LazySource` (`core.rs`) | Sources include fixpoint, explicit semantic computation, test-only semantic thunk, classified external host call, reflection computation, access arguments, application, builtin call, net construction/computation, and function call. Terminal `LazyResult` contains evaluated value or failure. | `source` is replaceable only under its mutex; `result` is one-write and published before source removal. Lock order is result check, source mutex, result recheck; destruction occurs after unlock. | I4C reports the terminal result when published, otherwise one stable source snapshot; it never invokes a producer. `LazyValue::cache` is source-latched as the sole terminal writer. I5 replaces the adapter and adds the structural gateway. Host callbacks remain for I10A. |
| `PromiseCell` (`core.rs`) | One successful `RuntimeValueRoot`; failure; weak/coordinator producer state and subscriptions. | Assignment and producer are one-write. Coordinator mutation admission encloses task-owned publication; resolver publication uses its local path. Notifications occur after guarded publication. | I4C reports a successful assignment's core value or its failure; producer/subscription companions are **C** and edge-free. `publish` and `publish_guarded` are latched assignment writers. I5 moves the assignment to a managed edge and adds its gateway. |
| `MetadataCarrier` (`core.rs`) | One `Value`. | Immutable after construction. | I4C reports exactly one edge; no post-construction gateway. I6 installs the managed visitor. |
| `BuiltinCall`, `Access`, `FunctionCall`, `LazyApplication` (`core.rs`) | Supplied function, arguments, and/or path leaf data. | Immutable `Arc` payloads; thread-safe. | I4C reports exact ordered semantic values. I5 migrates the function-call net identity and transitive walk; I6 installs remaining managed payloads. |
| `FixpointComputation` (`core.rs`) | Function or object-instance `Value`. | Immutable. | I4C reports exactly one value; I6 installs the managed visitor. |
| `ReflectionComputation` and gate target (`core.rs`) | The managed-facing computation carries an edge-free external-owner handle; that owner retains effect and optional target `RuntimeValueRoot`s plus an installed reservation failure. | Effect/target immutable; reservation/activation/task result are one-write lifecycle transitions. Cross-thread and may outlive original demand. | I4F.2b.2 moved effect, target, reservation, and failure ownership into the external registry, so the current compatibility adapter reports no direct managed edge. I6D.1 decides which semantic values move back into exact managed state while reservation activation/cancellation remains external lifecycle state. |
| `SemanticComputation` / test-only `SemanticThunk` / `HostCallProducer` (`core.rs`) | Production semantic computation stores a function pointer plus an explicit ordered `Arc<[Value]>`. Capture-bearing semantic closures exist only in test builds. A host-call producer accepts no evaluator context, returns a runtime root, and carries a mandatory source/capture record, but its genuinely external callback may retain host state or roots. | Immutable computation state/callbacks callable across workers. Host calls run in a distinct mutator-free lazy-machine poll phase. | I4C reports every production semantic capture in order. Test-only closure captures and host closure environments are deliberately not claimed traceable; I10A reconciles the external rooted ownership/backedges. |
| `FunctionCode`, `FunctionValue`, `NetValue` (`core.rs`) | Shared net plus scalar arity/capture state. | Immutable shells, synchronized net interior. | I4E compatibility adapters report exactly one existing net identity and no scalar edge without inspecting or reducing it. I5 installs the managed net identity; I6 installs remaining function-shell visitors and I8 performs the final audit. |
| `CoreOperator` variants (`core_net.rs`) | Supplied values, function code, builtin call, applicable value; keys/path elements are leaves. | Immutable operator payloads stored in mutable nets. | I4E's compile-exhaustive adapter reports every supplied/direct value and the function-code net identity without invoking the operator. I5 installs the transitive trace and every insertion/replacement gateway; I8 reconciles the final vocabulary. |
| `CoreValues`, `RuntimeValueCache`, admitted extension entries (`core.rs`, `core/runtime_cache.rs`) | Cached singleton values and admitted type-erased `Arc<T>` compiler caches such as `GCompilerValues`. | Runtime-long-lived; the seven-field core set is built and installed once as one complete immutable bundle. The extension map is replaceable under mutex with harmless duplicate construction, but only a recorded `RuntimeCacheFamily` may cross its private `Any` boundary. Candidate construction, root validation, and loser destruction occur outside the mutex. | I4F.1b.1 stores every `CoreValues` field as a `RuntimeValueRoot`, with no factory/domain backedge or partial publication. I4F.1b.2 validates every enumerated cache root against the owning runtime before complete entry publication and source-latches every admitted family; the compiler effect-map gateway preserves that provenance after publication. I4F.2 registers those surfaces at the managed-root switch. I9 audits lifecycle/backedges; I10 rechecks opaque containment. |

## Persistent Collections

`List`, `ListNode`, `ListChunk`, `SharedSlice`, `FingerList`, and
`RedBlackTreeMapSync<Key, Value>` are immutable persistent structures. Their
shared spines are ordinary Rust `Arc` ownership today. I4D's implemented
compatibility adapters trace RPDS and FingerTree contents through their public
iterators, even if this revisits a shared spine more than once. Glam's
unbalanced `ListNode::Concat` shell uses a small explicit trace worklist; lazy
and promise thunks are reported as edges and are never forced by tracing.
Bytes chunks and empty nodes are leaves. Recursive `Key` structures are walked
iteratively for exact policy and work accounting, but contain no semantic
edge. Logical counters record list nodes, finger chunks, segments, values,
thunks, map entries, and key nodes without deduplicating shared storage.

No element insertion occurs after a persistent node is published, so no
post-construction mutation gateway is required unless I7 later chooses managed
mutable spines. I7 replaces these compatibility adapters while retaining the
same logical edge policy and measures whether duplicate shared-spine visits
justify a physical collector-aware representation.

Collection constructors in `Values`, `CoreValueFactory`, evaluator builtins,
the syntax compiler, macro expansion, reflection stores, and net operators
must all run inside the same-runtime mutator region by I3. Existing persistent
collection libraries must never receive an unrooted managed pointer while a
safepoint is possible.

## Interaction-Net Graph

The core specialization fixes `Data = core::Value`,
`Operator = CoreOperator`, and `WaitToken = CoreWaitToken`.

| Owner | Edges and mutation | Synchronization / visitor / phase |
| --- | --- | --- |
| `InteractionNet<CoreSpecialization>` template | Immutable node array; data nodes contain `Value`, operator nodes contain `CoreOperator`. | I5B composes the exact template visitor into the transitive walk; construction-local until published. I8 performs the final audit. |
| `CoreRuntimeNet` authority facade (introduced in I3D.3) | Private core handle; I5D changes its internal identity from the generic external owner to `Gc<CoreRuntimeNetCell>` or the equivalent final managed type. | Every ordinary operation which locks and inspects or mutates semantic contents requires matching `RuntimeValueAccess`. I4E adds a bounded, read-only semantic-payload view and re-qualifies reported source identities to the same value domain. The managed handle does not escape unrooted into durable state. |
| `CoreRuntimeNetCell` (introduced in I5) | One managed outer allocation owning the semantic mutex, revisions, normalization/subscriber state, and ordinary `RuntimeNet<CoreSpecialization>` containers. | One net mutex is the exclusive graph mutation boundary. No callback or destruction while locked. Exact trace uses `try_lock` under exclusive collection; passive managed destruction drops only ordinary Rust resources. No per-agent GC allocation. |
| Generic `SharedRuntimeNetInner` / `SharedRuntimeNetState` | External owner retained for generic/non-core specializations and tests after the I5C.1 owner seam. | Shares topology and synchronization behavior without depending on `glam_gc`; it is not the production core-net owner after I5D. |
| `RuntimeNet`, `RuntimeEntry`, `RuntimeNode` | Data/operator values, ports, active-pair and cursor state. | I4E's generic logical walk reports data, operators, retained source identities, and specialization stuck reasons without mutation or work claims. Free mutation remains under the owning net mutex and core authority facade. Every value-installing rewrite requires the I5 gateway; the exact visitor uses `try_lock` under exclusive collection and I8 performs the final audit. |
| `CopyState`, `CursorClaim`, `PreparedCopySource`, `FrontierObservation` | Source-net references and cursor topology, abstracted from the generic external owner in I5C.1. | I4E reports source identities owned by durable copy and blocked-dependency state but does not follow their frontiers; transient claim/preparation handles remain outside the stored runtime walk. Production core source references become exact managed outer-cell edges in I5D. Copy topology remains hierarchical, while value-level paths may form collectible cycles through any net. |
| `NormalizationRequest`, `RequestRoot`, `Cursor`, `ActivePair`, `ResumeCursorDependency`, `NetDriverWorklist` (`eval/net.rs`) | Temporary/shared runtime-net handles. | Poll-orchestration owners; direct graph/value access passes through the core authority facade within bounded evaluator scopes. I3D.3 replaces manually paired call/cursor claim transitions with bracketed or lifetime-bound claims which cannot enter parked machine state. |
| Active-pair/cursor claim guards and dispositions (introduced in I3D.3) | One in-flight claim identity and owned data needed to publish resumed, blocked, stable, failed, or released state. | Private scope-bound T. A claim is consumed before semantic parking; unwind republishes a safe releasable state and disturbance. It is never a registered root or durable machine owner. |

## Registered Root and Transient Owner Inventory

The following named types are exhaustive for source-visible fields containing
`core::Value`, public `Value`, `RuntimeValueRoot`, `EvaluatedValue`, a store
snapshot, or a diagnostic which owns those values as of this ledger. A grouped
row shares one ownership policy; every listed type remains individually in the
inventory.

| Area | Types | Classification, lifetime, and migration |
| --- | --- | --- |
| Runtime value domain | `RuntimeValueDomain`, `CoreValueFactory`, `CoreValueAllocationScope`, `RuntimeValueAccess`, `CoreValueAllocator`, `Values`, `RuntimeValueFactory`, `RuntimeSharedResources` | Authorized strong value-domain leases under I1B. The domain owns one heap with an immutable construction-time `CollectionPolicy`, plus IDs, cache, and weak coordinator binding; it is never managed by its own heap. I1C admits allocation only through a higher-ranked factory scope; each typed allocator borrows the current mutator and cannot escape, while published collector roots remain non-owning. I3A.1 layers exact domain provenance over that scope as a thread-bound `RuntimeValueAccess`; subsystem evaluator views derive from it rather than manufacturing another heap entry. Integration remains `NoAuto` through I12A. I12B.0 may select a different policy only for newly constructed runtimes; no live setter or policy transition exists. Cached payloads cannot retain a factory/domain backedge. I4F installs the complete canonical root bundle after domain construction, closes extension attachments, and registers both surfaces. I9/I10 audit lifecycle and containment. |
| Public root facade | `RuntimeValueRoot`, `api::Value`, `EvaluatedValue`, `PromiseResolver`, `EffectTokenDomain`, `EffectTokenDomainState` | R. Public/runtime-long-lived and cross-thread, but non-owning with respect to the value domain. `RuntimeValueRoot` now implements I2's selected opaque representation: its private inline arm carries an `i64` plus a weak observer, and its managed arm shares one registered collector root cell; neither arm retains the domain. The public bare-handle contract supplies no equality, ordering, hashing, kind, provenance, liveness, or identity observation. A live matching runtime service authorizes fallible structural comparison, kind inspection, rendering, and owned extraction. `EvaluatedValue` remains the same root plus a static WHNF witness and weak observer rather than a second root; only owned host data may escape an access scope. Host dictionary/set keys must be obtained through authorized semantic observation rather than from the bare handle. Public construction uses one bounded `ScopedValues` carrier, every durable owner adopted the facade in I4F.1, and I4F.2 completed the registered-root production switch. The final compact tag layout remains deferred. `PromiseResolver` remains an external active-RAII owner: its idempotent retirement/`Drop` fallback preserves unresolved-promise failure and is audited in I5C/I9F. Generic effect-token domain payloads remain explicit external root owners. Any promise coordination split from its managed cell is separately **C** and contains no value root. |
| External evaluation lifecycle | `EvaluationSession`, `ClientDemandHandle`, `PendingReflectionTask`, plus any source-inventoried successor | R or capability-owning external Rust state, never M. Their current `Drop` paths close a session, abandon demand, or cancel an unactivated task through idempotent terminal operations. They may retain exact roots and authorized runtime capabilities, but no managed allocation may reach them. I9F records the concrete capability, root, lock/callback, and terminal contracts. |
| Assembly facade | `AssemblerReflectionHost`, `CompilationExecution`, `ReasoningSession`, `CompileSetup`, `BuiltModule`, `ReasoningVolume`, `Assembler`, `DiagnosticAttachment`, `AssemblerBuilder`, `ModuleBuilder` | R for stored public roots/diagnostics; T only for setup values proven bounded to one I3 scope. I4F.1 converted every parked setup/compiler field; I9 audits lifecycle and retirement. |
| Diagnostics | `Diagnostic`, `DiagnosticEvent`, `DiagnosticBusState`, `DiagnosticBusInner`, `DiagnosticBus`, `DiagnosticIngressInner`, `DiagnosticIngress`, `DiagnosticSubscription`, `DiagnosticSubscriptionInner`, `Error`, `ReasoningFailure` | R, closed as registered root surfaces by I4F.1/I4F.2. Buses/callbacks are external root owners; events retain emission and origin public roots until event retirement. Failure records retain their existing root-shaped errors. Weak ingress/subscription back-references remain non-owning, and subscriber callbacks run outside locks and mutation admission. I9 audits them. |
| Runtime events | `RuntimeInputRecord`, `RuntimeOutputIntent`, `RuntimeDeliveryRecord`, `RuntimeEventSnapshot`, `RuntimeEventJournal`, `RuntimePreparedInput`, `RuntimeDeliveryTicket`, `RuntimeDiagnosticRoute` | R, closed as registered root surfaces by I4F.1/I4F.2. Persistent input snapshots and identified queued/running deliveries retain `RuntimeValueRoot` across threads and settlement. Input conversion precedes admission; output callbacks receive a retained public root after locks. Weak endpoint ownership remains non-owning. I9 audits them. |
| Readiness/reporting | `QuiescenceSnapshot`, `QuiescenceReport`, `DeadlockSnapshot`, `RuntimeDeadlockWork`, `EvaluationSessionReport`, `EvaluationUnfinishedTask` | R. Host-visible snapshots may outlive sessions/runtime facade. Failures and store snapshots use registered root surfaces after I4F; I9 audits retirement. I12A.0 adds the authoritative GC operational-activity count/revision to readiness admission and validation, plus a durable disposition for an inactive pending finalizer batch. Collector statistics remain observational and never substitute for that runtime state. |
| Reflection store/protocol | `State`, `Set`, `Rewrite`, `StoreSnapshot`, `StoreJournal`, `ReflectionStore`, `Scoped`, `HostSnapshot`, `TaskCommit`, `Transaction`, `ReflectionJournal`, `QueryRead` | R. Store roots are persistent public roots; `ReflectionStore` and `StoreSnapshot` also retain an authorized factory/domain lease while they can construct transaction/query values. Journals are transaction-local snapshots/edits; store mutation uses runtime mutation admission then store/event mutex, with wakes/destruction after unlock. I4F closed concrete protocol fields, reusable request journals, decoded task states, and query reads as registered public/runtime roots; generic specialization state retains an explicit implementor-owned root contract, and pending-task/query capabilities delegate to their inventoried lifecycle owners. I9 audits them. |
| Reflection lifecycle/search | `EffectRun`, `IsolatedTaskHost`, `IsolatedSearchBranch`, `IsolatedSearchBlock`, `IsolatedEffectSearch` | R/T. Runs and isolated search retain registered public roots while active; same-runtime only. I3 establishes scoped access. I4F preserves coordinator `RuntimeFailureRoot` identity in lifecycle publication, roots bounded direct-run failures before returning them, roots blocked and terminal search failures before poll publication, verifies branch and journal retention, and treats the nested `EffectTask` as the separately inventoried machine owner. I9 audits lifecycle. |
| Reflection machine | `EffectTask`, `ContextualValueEffectTask`, `Branch`, `Deliver`, `Apply`, `CutFrame`, `TaskBlock`, `FixRoot`, `ActiveFix`, `Restore`, `ResetFrame` | R/T with the durable owner row closed. One claimed machine is exclusively mutable outside coordinator locks and may move between workers. I3A.4 makes successful machine completion root-shaped and preserves an effect result's existing runtime root. I4F.1d.3b-d root the effect API, diagnostic context, failures, branch state, work/outcomes/cuts, captured control, reset frames, and shared fixpoint functions; promise handles retain their I5 lifecycle contract. I4F.1d.3e makes rooted `Request` the sole evaluator-to-interpreter handoff, removes the redundant phase-root bundle, and confines raw request/fusion values to one bounded evaluator step. I4F.2 registers those root surfaces and retires their obsolete projections. I3D.2 alternates callback-free evaluator scopes and callback-bearing interpreter phases; only roots cross that boundary. I9 audits storage. |
| Evaluation coordinator | `EvaluationDemandState`, `ClaimedDemandSession`, `SettlementObligations`, `TaskOwnedPromiseObligation`, `DeferredLazyCycleMember`, `DeferredWorkRelease`, `RuntimeSettlementRelease`, `SparkDemand`, `ClientDemandWork`, `EvaluationTaskBlock`, `PromiseProducerObligation`, `LocalPromiseObligation`, `LocalPromiseOwner`, `PendingReflectionTaskInner` | R/T. `EvaluationDemandState` retains an authorized factory/domain lease; the coordinator registry and parked spark/client-demand routing are weak. I3A.2 upgrades the indexed route into one temporary `ClaimedDemandSession` before detaching reflection, deferred, client-demand, or spark work, rejects session/runtime mismatch before polling, and releases the route with the claim. I3A.3 derives the poll context only after detachment and centralizes task, client-demand, and spark polling in `evaluation/pump.rs`; executor workers own no second admission path. I3A.4 requires a `RuntimeValueRoot` at machine completion and publishes it without late reconstruction. An opaque parked machine may still deliberately own an authorized `EvalContext`; the coordinator envelope adds no second domain lease. Coordinator state owns parked work and registered root surfaces; weak promise cells are non-owning. State changes use mutation admission plus one component mutex; callbacks/drop happen after unlock. I4F completed the durable root conversion; I5 migrates managed interiors, and I9 audits lifecycle. |
| Evaluator machines | `EvaluationPollContext`, `EvaluatorStepContext`, `EvaluationValueAccess`, `LazyTaskMachine`, `LazyTaskWork`, `PromiseFollower`, `PromiseFollowerState`, annotation builtin state (`AssertUnit`, `MetadataPure`, `MetadataReflection`, `Reflection`, `Seq`, `Spark`, `Context`, `Valid`), net-construction `Data`/`NetConstructionMachine`, pattern `Found` | T. I3A.1 fixes the authority shape: a poll context carries no active mutator, while its derived value-access view is lifetime-bound, neither `Send` nor `Sync`, and cannot enter durable state. I3A.3 passes one claim-derived, non-extractable shared poll context through every type-erased machine poll. I3B.1a adds the intervening thread-bound evaluator-step context: it may span orchestration because it contains no mutator, but opens `EvaluationValueAccess` only for callback-free closures. Its claimed strong demand route exists only on the scheduler stack; I3B.1b adds one centralized direct-compatibility admission for inventoried I3D/I3E callers. The value/application/sequence spine, client demand, lazy/promise follower machines, and ordinary builtin families use the step carrier. I3B.1c closes the builtin partition: only effects, strategies, nets, and provenance downgrade at dispatch, while reflection and metadata-reflection annotations cross named durable handoffs after scoped recognition/validation. A machine poll is orchestration, not one mutator lifetime, and no active value access crosses those seams. `Complete` and every durable handoff are registered root-shaped after I4F. Managed recursive identities arrive atomically in I5; remaining managed interiors arrive in I6 and I8 performs the final net audit. |
| Compiler API | `ModuleLoadArgs`, `CompileContext`, `CompileDiagnosticEmitter`, `ModuleLoader`, `BinaryFileLoader` | T/D. Compile calls are bounded. Loader callbacks run outside inherited mutator access; deferred calls name their capture policy and carry prior/final definitions as roots. I10A still reconciles external callback backedges. |
| `.g` compiler cache | `BuiltinModule`, `GCompilerValues`, `CachedDiagnosticFormatter` | R. The runtime extension cache is long-lived and cross-thread. I4F.1b closed the arbitrary-`Any` boundary with stable admitted-family records, compile-exhaustive root visitors, same-runtime validation before publication, and a checked compiler effect-map mutation gateway. Every retained value is an exact registered root surface after I4F.2. I9 audits lifecycle; I10 rechecks erasure/opaque containment. |
| `.g` lowering/resolution | `LoweredSource`, `g_syntax::Diagnostic`, `MacroSnapshot`, `MacroJournal`, `MacroRun`, `MacroFailure`, `ModuleLowerer`, `ResolvedNetLowerer`, `ResolvedDoBlock`, `EffectBind`, `ValueBind`, `Then`, `ResolvedBindings` | T. Compilation/macro-run scoped values; mutator scope in I3 and no parking unrooted across evaluation. |
| `.g` lexical/parser | `LexedSource`, `Lexer`, `DeclarationMacroWork`, `StagedSourceParser`, `ParsedSource`, `InspectedSource`, `ParseSession` | T. Embedded source data and diagnostics are bounded by source compilation, but macro evaluation can suspend; I3. |
| CLI/configuration binary | `CliCaseExplanation`, `CliCompletion`, `ExpectationEvidence`, `CliJournal`, `CliSearchResult`, `SuccessfulBranch`, `TokenRun`, `CliError`, `CliExpansion`, `MainJournal`, `LoggerTaskHost`, `LoggerRun`, `LogHost`, `LoggerSupervisor`, `PreparedAssembly`, `LoadedConfiguration`, `DefaultLogger` | R/T outside the library crate. Public roots and diagnostics are retained through runtime callbacks/sessions. I3 supplies bounded evaluation entries, I4F converted every retained value owner, and I9 audits lifecycle. |
| Public net builder | `NetBind`, `NetCopy`, `NetBuilder` | T. Borrowed ports and a construction-local core net builder; cannot cross completion of construction. I3/I8. |

## Type-Erased Closure and Callback Inventory

| Family | Can capture values? | Boundary decision |
| --- | --- | --- |
| `SemanticComputation` / test-only `SemanticThunk` | Production captures are an explicit ordered `Arc<[Value]>`; only test scaffolding can close over raw values. | Exact semantic capture state selected by I4B. I5 traces it without inspecting a closure environment; I10 confirms no production test escape. |
| `HostCallProducer` | Host state and same-runtime roots, but no inherited evaluator context. | The deferred host call runs in its own mutator-free poll phase, carries a mandatory source/capture record, and must publish a runtime root. I10A owns final external-backedge reconciliation. |
| `ModuleLoader`, `BinaryFileLoader`, `CompileDiagnosticEmitter` | Yes; supplied by compiler setup and may capture core/public values. | External-demand/interpreter callbacks run without inherited mutator access. Any suspension/caching/capture requires exact same-runtime roots; audit concrete constructors in I4B. |
| `TaskStatusPublisher` and reflection launch/host closures | Indirectly; status, query, factory, or host state can retain roots. | Externally owned captures use registered root surfaces after I4F. A host companion associated only with a managed task is instead **C** and may capture no value. I9/I10 audit the boundary. |
| Runtime input converter and output decoder/callback | Yes, arbitrary host state and public values. | External adapter. Authoritative runtime buffers contain registered `RuntimeValueRoot`; callbacks run after locks and receive retained public roots. I9 audits retirement. |
| Diagnostic subscribers | Yes, arbitrary host state/public roots. | The external owner uses registered root captures after I4F; subscriptions use weak bus back-references and callbacks run outside locks. I9/I10 audit retirement and containment. |
| Notification/probe `FnOnce` closures | Normally IDs/wakers only; tests may capture fixtures. | Must not become a graph-retention mechanism. Audit constructors in I10. |

## Opaque Payload Families

| Payload | Managed edge status | Decision |
| --- | --- | --- |
| `EffectToken<T>` (`api/value.rs`) | Token contains ID and weak domain only; no core/managed edge. Generic domain payload remains outside the opaque token and may own public roots. | Approved leaf token. A value-bearing domain is an **R** external owner; an edge-free notification/identity domain may be **C**. |
| `ConstructionPort` (`eval/builtins/net/construction.rs`) | Brand and port ID only. | Approved leaf token. |
| `TaskHandleCell` (`reflection/requests.rs`) | Runtime ID, task handle, query handle; no raw core value. Handles reach coordinator/store obligations, not a managed pointer. | Approved external capability; re-audit in I9. |
| `CompilationOrigin` (`diagnostic.rs`) | Stores non-value `CompilationTrace`; constructs its diagnostic value on inspected access. | Approved edge-free provenance payload under I4B. |
| `OpaqueValue::new<T: OpaquePayloadFamily>` | Only the four source-latched production families can cross type erasure. Current payloads contain no direct `Gc`, raw `Value`, or `RuntimeValueRoot`. | I4B private unsafe admission plus mandatory family record. Root-bearing payloads are currently rejected. I10B.0 decides whether the bootstrap remains external-only or adds a distinct sealed managed arm; active external families retain idempotent retirement/RAII review under I9F/I10. |

An opaque value may not contain `Gc<T>`, an unrooted recursive core value, or a
root belonging to another runtime. The collector will not inspect `Any` or a
closure environment for hidden pointers.

## Managed Destruction and External Retirement

- A production managed `Drop` receives no runtime, value-domain, heap,
  evaluator, scheduler, diagnostic, or event capability. This applies through
  fields as well as direct `Drop` implementations: releasing the last `Arc`
  from a managed payload must not invoke active runtime behavior indirectly.
- Managed destruction may release ordinary Rust resources only. It must not
  observe or preserve any `Gc` edge held by the dying representation.
- State requiring cancellation, abandonment, notification, logging, or other
  active cleanup remains in an external owner holding exact public roots and,
  where required, an independently owned runtime/value-domain capability. The
  owner is unreachable from the managed graph, exposes an explicit idempotent
  retirement operation, and may invoke that same operation from ordinary Rust
  `Drop` when existing scope-exit semantics require it. This is active external
  RAII, not managed finalization; I9F inventories its capabilities, roots,
  lock/callback order, and terminal behavior.
- The generic collector may mechanically support a destructor that
  independently owns a `Heap`, but no Glam production managed family may carry
  that authority. Any proposed exception blocks Gate G2 pending a dedicated
  design review and ledger update.

## Synchronization and Managed-Edge Ledger

1. Runtime mutation admission is the outer settlement barrier. Component
   updates take at most one semantic mutex at a time; observation-epoch
   publication occurs after releasing that mutex but before releasing mutation
   admission. Wakes, callbacks, and value destruction happen afterward.
2. `LazyCell.result`, `PromiseCell.assignment`, reflection task installation,
   and promise producer installation are one-write edges. Their existing
   synchronization publishes fully initialized targets; once managed, the
   write sites also pass through the structural mutation gateway.
3. `LazyCell.source` is the only replaceable core-cell edge today. Its mutex
   guards removal; terminal result is published first. I5 routes result
   installation through the gateway and treats source removal as ordinary edge
   deletion.
4. Persistent list/dictionary nodes and immutable argument arrays need only
   their existing safe initialization before publication; they have no
   post-construction managed-edge gateway.
5. Interaction-net data/operator replacement is freely mutable under one net
   mutex and is the high-volume I8 mutation-gateway surface. Cursor/reduction
   helpers may not bypass it.
6. Reflection store, event state, coordinator ledgers, caches, and diagnostic
   buses are external Rust owners whose retained values use registered root
   surfaces after I4F. They
   are not interior managed pointers; updating them changes root membership
   rather than invoking an object-field mutation gateway. I9 audits their
   lifecycle but is never the first root-conversion phase. Any companion state
   split from an individual managed node is classified separately as **C** and
   has no such entry.
7. No trace visitor may call user code, allocate, lock unrelated components,
   or force a lazy/promise/net value. Visitors enumerate representation edges
   only.

## Semantic Verification Matrix

These tests latch the requested pre-migration behavior. They are deliberately
kept with the subsystem whose contract they exercise.

| Contract | Existing regression coverage |
| --- | --- |
| Production opaque inline-or-managed facade, weak provenance, transport-only traits, runtime-authorized observation, owned extraction, and nested scoped access | `public_values_use_inline_or_shared_registered_roots`; the `prepared_root_*` suite; public-facade negative trait latches; `public_value_facade_exposes_no_core_or_provenance_observer`. |
| I3 bounded evaluator authority, root-shaped poll outcomes, callback/wait separation, exact net claims, classified lazy producers, compiler/event regions, and multi-runtime admission | `all_managed_entries_have_bounded_mutator_regions`; `direct_evaluator_admission_has_one_internal_compatibility_gate`; `evaluation_machine_poll_boundary_inventory_is_complete`; `effect_interpreter_callbacks_do_not_inherit_evaluator_mutators`; `reflection_gate_reserves_inside_and_activates_outside_scope`; `compiler_root_and_projection_inventory_is_complete`; `event_delivery_invokes_callback_without_mutator`; `diagnostic_rendering_invokes_writer_without_mutator`; `runtime_tls_caches_remain_heap_qualified`; `worker_termination_releases_inactive_collector_caches`. |
| I4.0 private managed-family destruction admission, mandatory direct/transitive records, passive managed destruction, and external active-retirement separation | compile-time negative admission latches in `core::managed::tests`; `managed_family_collection_requires_completed_drop_record`; `managed_drop_has_no_runtime_or_heap_capability`; `external_raii_owner_is_not_reachable_from_managed_graph`. |
| I4 production managed-shell granularity, stable layout/drop admission, exhaustive current-variant dispatch, embedded leaf policy, rooted survival, and unrooted reclamation | `managed_value_node_family_contract_and_lifecycle`; `managed_value_node_dispatches_every_real_variant_as_zero_edge`; `managed_value_gateway_remains_private_to_the_root_representation`. |
| I4B closure, host-call, opaque-family, and active-owner containment | `closure_and_opaque_constructor_inventory_is_classified`; `semantic_computation_captures_are_explicit`; `external_host_call_requires_a_source_backed_record`; `host_call_capture_retires_only_during_external_registry_drain`; `opaque_payload_requires_matching_runtime_and_retires_during_registry_drain`; `active_value_destruction_frontiers_are_source_latched`; `every_real_value_variant_has_passive_managed_destruction`. |
| I4C recursive compatibility edge vocabulary, ordered payload/failure visitation, lifecycle-edge exclusions, and no semantic work during visitation | `argument_and_application_visitors_enumerate_exact_edges`; `compatibility_recursive_payload_visitors_enumerate_exact_edges`; `shared_cyclic_failure_context_traces_exactly`; `failure_trace_invokes_no_semantic_service`; `recursive_edge_mutations_use_representation_gateways`. |
| I4D non-forcing persistent collection adapters, logical work accounting, shared-spine behavior, and closed collection proof | `persistent_adapter_traces_empty_singleton_and_shared_spines`; `persistent_adapter_cycle_reclaims_in_isolated_heap`. |
| I4E non-reducing net/function identity adapters, exact runtime payload classification, revision/materialization preservation, and closed collection proof | `core_operator_adapter_enumerates_every_value_and_net_payload`; `net_value_adapter_traces_without_reduction_or_materialization`; `net_value_adapter_cycle_marks_exactly`. |
| I4F.1a durable-owner schema, production declaration drift latch, semantic owner assignment, and explicit open conversion rows | `durable_value_owner_inventory_is_complete`. |
| I4F cache/compiler owner closure, complete publication, same-runtime validation, and retirement | `canonical_cache_publishes_one_complete_root_bundle`; `canonical_cache_releases_with_the_last_value_domain_owner`; `runtime_cache_family_source_inventory_is_complete`; `runtime_cache_rejects_a_root_from_another_runtime_before_publication`; `runtime_cache_retires_an_admitted_owner_with_the_value_domain`; `compiler_durable_owner_inventory_is_compile_exhaustive`; `compiler_cache_publishes_complete_rooted_bundle`. |
| I4F coordinator, report, reflection, diagnostic, event, delivery, compiler, macro, binary-host, and synchronized-net root ownership | the I4F.2e focused owner fixtures, including `terminal_task_wait_root_survives_collection_until_handle_drop`; `failure_ledger_root_survives_collection_after_report_and_handle_drop`; `query_result_remains_rooted_after_store_and_handle_retirement`; `diagnostic_events_retain_emission_and_origin_roots_until_retirement`; `runtime_input_roots_follow_buffer_journal_and_committed_result_owners`; `delivery_failure_roots_follow_ledger_and_snapshot_owners`; `module_load_arguments_retain_definition_roots_until_handoff_retires`; `macro_results_and_failures_retain_public_values_until_retirement`; `shared_core_net_payload_survives_managed_owner_collection`. |
| I4F production switch and deletion closure: exact root-publication owners, no authority-free core escape, no public representation-derived relation, and no obsolete prototype | `registered_runtime_root_publication_inventory_is_complete`; `public_value_switch_inventory_has_no_compatibility_escape`; public `Value`/`EvaluatedValue` and private prepared-root negative trait latches; `durable_value_owner_inventory_is_complete`. |
| Cross-runtime rejection | `public_value_factories_reject_foreign_composite_members`; `assembler_boundaries_reject_foreign_values_before_evaluation_or_storage`; `runtime_input_endpoints_are_local_monotonic_capabilities`. |
| Fulfilled/unfulfilled lazy and resolver promise | `value_evaluator_caches_lazy_success_and_preserves_structured_failure`; `value_evaluator_resumes_a_retained_resolver_promise_subscription`; `promised_assignments_retain_deferred_aliases`. |
| Pure lazy cycle | `a_lazy_task_that_waits_on_itself_is_poisoned_as_a_cycle`; `concurrently_demanded_lazy_tasks_share_one_two_node_cycle_failure`; `two_sessions_share_and_retire_one_pure_lazy_cycle_failure`. |
| Promise/lazy cycle | `promise_only_cycle_remains_blocked_without_poisoning_its_assignment`; `mixed_promise_lazy_cycle_remains_retryable_without_poisoning_the_lazy`; `a_cross_session_promise_lazy_cycle_remains_unpoisoned`. |
| Metadata/collection deferred cycle and graph identity | `core::tests::metadata_and_collections_can_participate_in_a_deferred_value_cycle`; `metadata_carriers_hide_unit_and_associated_metadata`; metadata update reorder/copy tests in `eval::tests`. |
| Function/collection/net recursive graph | `evaluates_recursive_dictionary_net`; `compiled_function_values_reuse_one_shared_interaction_net`; `curried_function_partial_application_retains_a_shared_stage`; net cursor/copy runtime tests. |
| Same-runtime worker transfer | `workers_force_sparks_and_poll_ready_reflection_tasks` and the evaluation executor worker tests. |
| Runtime facade drop with escaped value/resources | `value_evaluator_returns_a_runtime_rooted_whnf_witness`; `runtime_shared_resources_do_not_retain_runtime_lifecycle_owners`; `public_values_retain_only_the_runtime_value_domain`; `bare_public_values_do_not_retain_the_runtime_value_domain`; `runtime_value_domain_has_no_scheduler_or_profile_backedge`; `evaluation_context_retains_runtime_cache_and_profile_without_a_cycle`; `retained_reflection_profile_keeps_only_shared_resources_alive`; `compiler_cache_does_not_form_a_value_domain_cycle`. |
| Reflection/task state after owner close | `blocked_machine_context_does_not_retain_its_owner_lease`; `task_handle_acknowledges_terminal_failure_after_owner_lease_closes`; `task_handle_cancellation_is_harmless_after_owner_closure`; scheduled effect lifecycle tests. |
| Store snapshot retention | `runtime_event_snapshots_preserve_persistent_input_roots`; reflection store persistent-snapshot tests. |
| Settlement retention | `ready_settlement_publishes_exited_once_and_retains_exit_errors`; `forced_deadlock_settlement_preserves_exits_and_kills_other_participants`; coordinator terminal-settlement tests. |
| Diagnostic/event delivery retention | `output_payload_is_retained_through_callback_and_dropped_after_locks`; `running_delivery_retains_shared_resources_until_terminal_publication`; `diagnostic_bus_and_ingress_do_not_retain_the_runtime`; settled-report rendering tests. |

The matrix is a migration checklist: when a representation moves, its named
tests must still pass under forced collection at the phase named in the main
plan. It does not substitute for the I11 whole-graph forced-collection suite.

## Gate G2 Blockers and Reconciliation

I4B and I4F resolved the original raw `CompilationOrigin`, production
semantic-thunk, unrestricted opaque-constructor, type-erased cache, and durable
root-surface blockers. The remaining findings block collection rather than
receiving a guessing/conservative classification:

1. External compiler/host callback environments remain opaque Rust owners.
   Each deferred import now carries an explicit source/capture record and
   rooted Glam arguments/results; I10A must still prove that external roots do
   not hide an internally owned backedge.
2. I5 must replace lazy, promise, and production core-net recursive identities
   atomically, with one transitive compatibility walk and exact synchronized
   net trace, durable-handle, and value-installing mutation-gateway
   inventories. I8 performs the final post-cutover audit.
3. Public opaque construction needs the I10B.0 representation decision and a
   closed leaf/root registration boundary. If that review selects a managed
   arm, every admitted concrete family also needs its own exact stable ledger
   record, scoped-access proof, and I4.0 destruction admission before Gate G2.

As each M family receives its managed representation, append the stable
reconciliation record defined above. In particular, record its Rust
type/source owner, reviewed edge visitor, requested extent and Rust layout,
allocator-discovery acceptance, drop/finalization policy, mutation gateway,
and external-root classification. Do not record its metadata address, dense
class ID, or derived run geometry here.

Before Gate G2, re-run the source inventory for new value fields,
`Arc<dyn Fn...>`, `Any`, opaque constructors, and interaction-net payloads;
match every result to one stable family record. Separately run the collector's
layout/class verification for every requested extent used by those families.
An unmatched source result, incomplete stable record, rejected layout, or
missing collector test keeps Gate G2 closed.
