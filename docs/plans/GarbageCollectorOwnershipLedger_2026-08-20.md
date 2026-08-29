# Glam GC Ownership and Mutation Ledger — 2026-08-20

Status: Phase I0's pre-GC inventory, Phase I2's public-root contract, and
I3A.1's scoped authority shape and I3A.2's claim-owned domain routing are
complete. Stable integration facts are reconciled when each representation
family receives its concrete managed wrapper and trace implementation.
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
| `NetValue`, `FunctionCode`, `FunctionValue` | 8/8, 24/8, 16/8 | Net/function graph integration in I6/I8. |
| `BuiltinCall`, `EvaluationFailure` | 24/8, 80/8 | Exact visitors in I4/I6. |
| `List`, `ListThunk` | 8/8, 16/8 | Logical persistent-container trace in I7. |
| `RuntimeValueRoot`, `CoreOperator` | 72/8, 96/8 | Root facade in I2/I9; net payload visitor in I8. |
| `ListNode`, `ListChunk`, `SharedSlice`, `FingerList` | 40/8, 40/8, 32/8, 16/8 | Existing external persistent spines, logically visited in I7. |
| `SharedRuntimeNet`, `SharedRuntimeNetInner`, `SharedRuntimeNetState` | 8/8, 256/8, 224/8 | Current synchronized owner/layout input. I8 replaces the production core owner with one managed outer cell; individual topology entries remain ordinary storage. |
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

## `core::Value` Variant Ledger

| Variant | Current owner and outgoing edges | Mutation, threading, and longevity | Exact visitor / mutation gateway / migration |
| --- | --- | --- | --- |
| `Atom` | Interned `Key`; no `Value` edge. | Immutable, `Send + Sync`, long-lived. | Leaf; no mutation gateway; I4. |
| `Number` | Number-owned integer/rational storage; no `Value` edge. | Immutable, thread-safe, long-lived. | Leaf initially; I4. |
| `Binary` | `bytes::Bytes`; no managed edge. | Immutable shared leaf. | Leaf/external allocation; I4. |
| `List` | Persistent list nodes; value chunks and lazy/promise thunks are outgoing edges. | Immutable/persistent and thread-safe; may escape all evaluator calls. | Logical item/thunk visitor, including shared spines; no post-publication mutation; I7. |
| `Dict` | `RedBlackTreeMapSync<Key, Value>`; every mapped value is an edge. Keys contain no live `Value` after conversion. | Immutable/persistent and thread-safe; may be runtime-global. | Logical entry visitor; no post-publication mutation; I7. |
| `Builtin` | Static enum only. | Immutable leaf. | Leaf; I4. |
| `PartialBuiltin` | `BuiltinCall.arguments: Arc<[Value]>`. | Immutable, shared across threads. | Visit every supplied argument; I6. |
| `Function` | `FunctionValue -> NetValue -> CoreRuntimeNet`; the net carries data/operator values. | Immutable shell over a managed synchronized core-net cell; long-lived. | Visit the managed net identity; I6/I8. |
| `Net` | `NetValue -> CoreRuntimeNet`. | Immutable shell over a managed outer cell containing freely mutable net state protected by that cell's mutex. | Exact locked outer-cell visitor and managed-edge gateways; I8. |
| `Lazy` | `Arc<LazyCell>`; source and terminal result graphs. | Identity-bearing, thread-safe, long-lived; source/result publication races are supported. | Managed cell visitor; one-write result plus replaceable source protocol; I5. |
| `Promised` | `Arc<PromiseCell>`; successful assignment root and producer obligation. | Identity-bearing, thread-safe, long-lived; assignment is one-write. | Managed cell visitor; assignment mutation gateway; I5. |
| `Metadata` | `MetadataCarrier.metadata: Arc<Value>`. | Immutable identity-bearing sealed value, thread-safe. | Visit exactly one metadata value; I6. |
| `Opaque` | `Arc<dyn Any + Send + Sync>`; payload-dependent. | Pointer-identity shell; arbitrary longevity/thread transfer. | I10B.0 is a hard review gate. Until it selects otherwise, opaque storage remains external: arbitrary `Any` is edge-free or owns audited same-runtime public roots. A possible managed arm must be a separate sealed exact representation outside `Any`, with one stable family record per admitted type. No managed opaque allocation or scoped managed downcast is authorized before the review. |

## Recursive Core Nodes

| Type and source | Outgoing edges | Mutation and synchronization | Drop / trace / mutation gateway / phase |
| --- | --- | --- | --- |
| `EvaluationFailure` (`core.rs`) | Emission `Value` or cycle IDs; `Arc<[Value]>` contexts. | Immutable, thread-safe, retained in task/report ledgers. | Visit emission and contexts; ordinary Rust drop initially; I6. |
| `LazyCell` / `LazySource` (`core.rs`) | Sources include fixpoint, deferred closure, reflection computation, access arguments, application, builtin call, net construction/computation, and function call. Terminal `LazyResult` contains evaluated value or failure. | `source` is replaceable only under its mutex; `result` is one-write and published before source removal. Lock order is result check, source mutex, result recheck; destruction occurs after unlock. | Exact source/result visitor. Initial construction needs no gateway; result publication uses one. Deferred/opaque captures block I5 until I4B. Managed drop required because source/result own Rust resources. |
| `PromiseCell` (`core.rs`) | One successful `RuntimeValueRoot`; failure; weak/coordinator producer state and subscriptions. | Assignment and producer are one-write. Coordinator mutation admission encloses task-owned publication; resolver publication uses its local path. Notifications occur after guarded publication. | Move the assignment into a traced managed edge rather than preserving its current registered-root representation. Any separated producer/subscription companion is **C** and edge-free. Active abandonment/notification belongs to an external explicitly retired owner; managed `Drop` is passive. Assignment gateway; I5. |
| `MetadataCarrier` (`core.rs`) | One `Value`. | Immutable after construction. | Visit one edge; no post-construction gateway; I6. |
| `BuiltinCall`, `Access`, `FunctionCall`, `LazyApplication` (`core.rs`) | Supplied function, arguments, and/or path leaf data. | Immutable `Arc` payloads; thread-safe. | Exact ordered visitors; I6. |
| `FixpointComputation` (`core.rs`) | Function or object-instance `Value`. | Immutable. | Visit one value; I6. |
| `ReflectionComputation` and gate target (`core.rs`) | Effect and gate target values; installed task result may carry a failure. | Effect/target immutable; reservation/activation/task result are one-write lifecycle transitions. Cross-thread and may outlive original demand. | Visit values and failure; I3D.1 reserves under pure evaluation and activates the launcher after leaving scoped access; task-result gateway if the node is managed; I6. |
| `DeferredComputation` (`core.rs`) | Type-erased `Arc<dyn Fn(&EvalContext) -> ...>` may capture arbitrary raw core values and currently conflates semantic thunks with host loaders. | Immutable closure, callable across workers. | **D:** cannot be traced or admitted uniformly. I3E.1 splits callback-free semantic thunks from deterministic external demands; I4B/I10 then contain or replace every captured-value closure. |
| `FunctionCode`, `FunctionValue`, `NetValue` (`core.rs`) | Shared net plus scalar arity/capture state. | Immutable shells, synchronized net interior. | Net visitor in I8; I6. |
| `CoreOperator` variants (`core_net.rs`) | Supplied values, function code, builtin call, applicable value; keys/path elements are leaves. | Immutable operator payloads stored in mutable nets. | Variant visitor; insertion/replacement gateways at net mutation sites; I8. |
| `CoreValues`, `RuntimeValueCache`, extension map (`core.rs`) | Cached singleton values and type-erased `Arc<T>` compiler caches such as `GCompilerValues`. | Runtime-long-lived; core set initialized once, extension map replaceable under mutex, harmless duplicate construction permitted. | I4F.1 converts every durable cached value to an exact registered root or exact managed cache edge and closes the type-erased registration boundary before the managed-root switch. I9 audits lifecycle/backedges; I10 rechecks opaque containment. |

## Persistent Collections

`List`, `ListNode`, `ListChunk`, `SharedSlice`, `FingerList`, and
`RedBlackTreeMapSync<Key, Value>` are immutable persistent structures. Their
shared spines are ordinary Rust `Arc` ownership today. The initial collector
will trace RPDS and FingerTree contents through their public iterators, which
already use explicit heap-backed traversal state, even if this revisits a
shared spine more than once. Glam's unbalanced `ListNode::Concat` shell uses a
small explicit trace worklist; lazy thunks are reported as edges and are never
forced by tracing. No element insertion occurs after a node is published, so
no post-construction mutation gateway is required unless I7 later chooses
managed mutable spines. `ListThunk::{Lazy, Promised}` visits its one deferred
cell. Bytes chunks and empty nodes are leaves.

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
| `InteractionNet<CoreSpecialization>` template | Immutable node array; data nodes contain `Value`, operator nodes contain `CoreOperator`. | Exact template visitor; construction-local until published; I8. |
| `CoreRuntimeNet` authority facade (introduced in I3D.3) | Private core handle; I8 changes its internal identity from the generic external owner to `Gc<CoreRuntimeNetCell>` or the equivalent final managed type. | Every ordinary operation which locks and inspects or mutates semantic contents requires matching `RuntimeValueAccess`; the managed handle does not escape unrooted into durable state. Generic interaction-net topology remains collector-independent. |
| `CoreRuntimeNetCell` (introduced in I8) | One managed outer allocation owning the semantic mutex, revisions, normalization/subscriber state, and ordinary `RuntimeNet<CoreSpecialization>` containers. | One net mutex is the exclusive graph mutation boundary. No callback or destruction while locked. Exact trace uses `try_lock` under exclusive collection; passive managed destruction drops only ordinary Rust resources. No per-agent GC allocation. |
| Generic `SharedRuntimeNetInner` / `SharedRuntimeNetState` | External owner retained for generic/non-core specializations and tests after the I8A owner seam. | Shares topology and synchronization behavior without depending on `glam_gc`; it is not the production core-net owner after I8B. |
| `RuntimeNet`, `RuntimeEntry`, `RuntimeNode` | Data/operator values, ports, active-pair and cursor state. | Free mutation only under the owning net mutex and core authority facade. Every value-installing rewrite requires the I8 mutation gateway. The exact visitor uses `try_lock` under exclusive collection; contention is an invariant defect because no ordinary scoped accessor can coexist. |
| `CopyState`, `CursorClaim`, `PreparedCopySource`, `FrontierObservation` | Source-net references and cursor topology, abstracted from the generic external owner in I8A. | Production core source references become exact managed outer-cell edges in I8B. Copy topology remains hierarchical, while value-level paths may form collectible cycles through any net. |
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
| Runtime value domain | `RuntimeValueDomain`, `CoreValueFactory`, `CoreValueAllocationScope`, `RuntimeValueAccess`, `CoreValueAllocator`, `Values`, `RuntimeValueFactory`, `RuntimeSharedResources` | Authorized strong value-domain leases under I1B. The domain owns one heap with an immutable construction-time `CollectionPolicy`, plus IDs, cache, and weak coordinator binding; it is never managed by its own heap. I1C admits allocation only through a higher-ranked factory scope; each typed allocator borrows the current mutator and cannot escape, while published collector roots remain non-owning. I3A.1 layers exact domain provenance over that scope as a thread-bound `RuntimeValueAccess`; subsystem evaluator views derive from it rather than manufacturing another heap entry. Integration remains `NoAuto` through I12A. I12B.0 may select a different policy only for newly constructed runtimes; no live setter or policy transition exists. Cached payloads cannot retain a factory/domain backedge. I4F.1 installs cache roots before managed values escape; I9/I10 audit lifecycle and containment. |
| Public root facade | `RuntimeValueRoot`, `api::Value`, `EvaluatedValue`, `PromiseResolver`, `EffectTokenDomain`, `EffectTokenDomainState` | R. Public/runtime-long-lived and cross-thread, but non-owning with respect to the value domain. I2A selected an opaque Glam wrapper whose private inline arm carries allocation-free data plus a weak domain witness and whose managed arm reuses the collector's existing root cell; neither arm retains the domain. I2B.1 restricts the selected handle contract to transport plus content-free debugging: it supplies no equality, ordering, hashing, kind, provenance, liveness, or identity observation. I2B.2 makes a borrowed live matching runtime service the sole authority for fallible structural comparison, kind inspection, rendering, and owned extraction. The evaluated facade remains the same root plus a static WHNF witness, not an authority or second root; only owned host data may escape an access scope. Host dictionary/set keys must be obtained through this authorized semantic observation rather than from the handle. I2C fixes and mechanically checks the complete compatibility-access baseline and assigns every owning module to I3 scoped access plus I4F durable-root/facade migration. The final tag layout and managed type-erasure gateway remain deferred, and production still uses `RuntimeValueRoot`. Every durable owner adopts the facade in I4F.1, and I4F.2 performs the production managed-root switch. `PromiseResolver` remains an external active-RAII owner: its idempotent retirement/`Drop` fallback preserves unresolved-promise failure and is audited in I5C/I9F. Generic effect-token domain payloads remain explicit external root owners. Any promise coordination split from its managed cell is separately **C** and contains no value root. |
| External evaluation lifecycle | `EvaluationSession`, `ClientDemandHandle`, `PendingReflectionTask`, plus any source-inventoried successor | R or capability-owning external Rust state, never M. Their current `Drop` paths close a session, abandon demand, or cancel an unactivated task through idempotent terminal operations. They may retain exact roots and authorized runtime capabilities, but no managed allocation may reach them. I9F records the concrete capability, root, lock/callback, and terminal contracts. |
| Assembly facade | `AssemblerReflectionHost`, `CompilationExecution`, `ReasoningSession`, `CompileSetup`, `BuiltModule`, `ReasoningVolume`, `Assembler`, `DiagnosticAttachment`, `AssemblerBuilder`, `ModuleBuilder` | R for stored public roots/diagnostics; T only for setup values proven bounded to one I3 scope. I4F.1 converts every parked setup/compiler field; I9 audits lifecycle and retirement. |
| Diagnostics | `Diagnostic`, `DiagnosticEvent`, `DiagnosticBusState`, `DiagnosticBusInner`, `DiagnosticBus`, `DiagnosticIngressInner`, `DiagnosticIngress`, `DiagnosticSubscription`, `DiagnosticSubscriptionInner`, `Error`, `ReasoningFailure` | R. Buses/callbacks are external root owners; events retain public roots until delivery/retirement. Weak back-references remain non-owning. Converted in I4F.1; audited in I9. |
| Runtime events | `RuntimeInputRecord`, `RuntimeOutputIntent`, `RuntimeDeliveryRecord`, `RuntimeEventSnapshot`, `RuntimeEventJournal`, `RuntimePreparedInput`, `RuntimeDeliveryTicket`, `RuntimeDiagnosticRoute` | R. Persistent input snapshots and identified deliveries retain roots across threads and settlement. Converted in I4F.1; audited in I9. |
| Readiness/reporting | `QuiescenceSnapshot`, `QuiescenceReport`, `DeadlockSnapshot`, `RuntimeDeadlockWork`, `EvaluationSessionReport`, `EvaluationUnfinishedTask` | R. Host-visible snapshots may outlive sessions/runtime facade. Failures and store snapshots are exact roots by I4F.1; I9 audits retirement. I12A.0 adds the authoritative GC operational-activity count/revision to readiness admission and validation, plus a durable disposition for an inactive pending finalizer batch. Collector statistics remain observational and never substitute for that runtime state. |
| Reflection store/protocol | `State`, `Set`, `Rewrite`, `StoreSnapshot`, `StoreJournal`, `ReflectionStore`, `Scoped`, `HostSnapshot`, `TaskCommit`, `Transaction`, `ReflectionJournal`, `QueryRead` | R. Store roots are persistent public roots; `ReflectionStore` and `StoreSnapshot` also retain an authorized factory/domain lease while they can construct transaction/query values. Journals are transaction-local snapshots/edits; store mutation uses runtime mutation admission then store/event mutex, with wakes/destruction after unlock. Converted in I4F.1; audited in I9. |
| Reflection lifecycle/search | `EffectRun`, `IsolatedTaskHost`, `IsolatedSearchBranch`, `IsolatedSearchBlock`, `IsolatedEffectSearch` | R/T. Runs and isolated search retain public roots while active; same-runtime only. I3 establishes scoped access, I4F.1 roots every retained value, and I9 audits lifecycle. |
| Reflection machine | `EffectTask`, `ContextualValueEffectTask`, `Branch`, `Deliver`, `Apply`, `CutFrame`, `TaskBlock`, `FixRoot`, `ActiveFix`, `Restore`, `ResetFrame` | T transitioning to exact machine-owned roots. One claimed machine is exclusively mutable outside coordinator locks and may move between workers. I3A.4 makes successful machine completion root-shaped and preserves an effect result's existing runtime root; parked interior fields become exact roots in I4F.1. I3D.2 alternates callback-free evaluator scopes and callback-bearing interpreter phases; only roots cross that boundary. I9 audits storage. |
| Evaluation coordinator | `EvaluationDemandState`, `ClaimedDemandSession`, `SettlementObligations`, `TaskOwnedPromiseObligation`, `DeferredLazyCycleMember`, `DeferredWorkRelease`, `RuntimeSettlementRelease`, `SparkDemand`, `ClientDemandWork`, `EvaluationTaskBlock`, `PromiseProducerObligation`, `LocalPromiseObligation`, `LocalPromiseOwner`, `PendingReflectionTaskInner` | R/T. `EvaluationDemandState` retains an authorized factory/domain lease; the coordinator registry and parked spark/client-demand routing are weak. I3A.2 upgrades the indexed route into one temporary `ClaimedDemandSession` before detaching reflection, deferred, client-demand, or spark work, rejects session/runtime mismatch before polling, and releases the route with the claim. I3A.3 derives the poll context only after detachment and centralizes task, client-demand, and spark polling in `evaluation/pump.rs`; executor workers own no second admission path. I3A.4 requires a `RuntimeValueRoot` at machine completion and publishes it without late reconstruction. An opaque parked machine may still deliberately own an authorized `EvalContext`; the coordinator envelope adds no second domain lease. Coordinator state owns parked work and registered roots; weak promise cells are non-owning. State changes use mutation admission plus one component mutex; callbacks/drop happen after unlock. I3 establishes the scope boundary, I4F.1 roots durable values, I5 migrates managed interiors, and I9 audits lifecycle. |
| Evaluator machines | `EvaluationPollContext`, `EvaluatorStepContext`, `EvaluationValueAccess`, `LazyTaskMachine`, `LazyTaskWork`, `PromiseFollower`, `PromiseFollowerState`, annotation builtin state (`AssertUnit`, `MetadataPure`, `MetadataReflection`, `Reflection`, `Seq`, `Spark`, `Context`, `Valid`), net-construction `Data`/`NetConstructionMachine`, pattern `Found` | T. I3A.1 fixes the authority shape: a poll context carries no active mutator, while its derived value-access view is lifetime-bound, neither `Send` nor `Sync`, and cannot enter durable state. I3A.3 passes one claim-derived, non-extractable shared poll context through every type-erased machine poll. I3B.1a adds the intervening thread-bound evaluator-step context: it may span orchestration because it contains no mutator, but opens `EvaluationValueAccess` only for callback-free closures. Its temporary strong demand route exists only on the scheduler stack. A machine poll is orchestration, not one mutator lifetime: existing opaque evaluator calls which may pump, wait, or invoke callbacks do not open access until I3B-I3D split them. I3A.4 makes `Complete` root-shaped at the type boundary and inventories promise/failure interiors for I5/I6. The current bare-result constructor is a compatibility seam which I3B moves inside a bounded access scope before I4F.2 introduces managed results. Managed interior edges arrive in I5/I6/I8 and update their exact visitors in the same checkpoint. |
| Compiler API | `ModuleLoadArgs`, `CompileContext`, `CompileDiagnosticEmitter`, `ModuleLoader`, `BinaryFileLoader` | T/D. Compile calls are bounded. Loader callbacks run outside inherited mutator access and return roots through deterministic external-demand gates; captured raw values still require containment in I4B/I10. |
| `.g` compiler cache | `BuiltinModule`, `GCompilerValues` | R/D before I4F.1. The runtime extension cache is long-lived and cross-thread; I4F.1 closes the `Any` registration boundary and converts every retained value to an exact runtime root/cache edge before the production switch. I9 audits lifecycle; I10 rechecks erasure/opaque containment. |
| `.g` lowering/resolution | `LoweredSource`, `g_syntax::Diagnostic`, `MacroSnapshot`, `MacroJournal`, `MacroRun`, `MacroFailure`, `ModuleLowerer`, `ResolvedNetLowerer`, `ResolvedDoBlock`, `EffectBind`, `ValueBind`, `Then`, `ResolvedBindings` | T. Compilation/macro-run scoped values; mutator scope in I3 and no parking unrooted across evaluation. |
| `.g` lexical/parser | `LexedSource`, `Lexer`, `DeclarationMacroWork`, `StagedSourceParser`, `ParsedSource`, `InspectedSource`, `ParseSession` | T. Embedded source data and diagnostics are bounded by source compilation, but macro evaluation can suspend; I3. |
| CLI/configuration binary | `CliCaseExplanation`, `CliCompletion`, `ExpectationEvidence`, `CliJournal`, `CliSearchResult`, `SuccessfulBranch`, `TokenRun`, `CliError`, `CliExpansion`, `MainJournal`, `LoggerTaskHost`, `LoggerRun`, `LogHost`, `LoggerSupervisor`, `PreparedAssembly`, `LoadedConfiguration`, `DefaultLogger` | R/T outside the library crate. Public roots and diagnostics are retained through runtime callbacks/sessions. I3 supplies bounded evaluation entries, I4F.1 converts every retained value owner, and I9 audits lifecycle. |
| Public net builder | `NetBind`, `NetCopy`, `NetBuilder` | T. Borrowed ports and a construction-local core net builder; cannot cross completion of construction. I3/I8. |

## Type-Erased Closure and Callback Inventory

| Family | Can capture values? | Boundary decision |
| --- | --- | --- |
| `DeferredComputation` | Yes, raw `core::Value` today. | D. I3E.1 separates semantic thunks from external demands; replace/contain captures in I4B/I10 and never conservatively trace a closure environment. |
| `ModuleLoader`, `BinaryFileLoader`, `CompileDiagnosticEmitter` | Yes; supplied by compiler setup and may capture core/public values. | External-demand/interpreter callbacks run without inherited mutator access. Any suspension/caching/capture requires exact same-runtime roots; audit concrete constructors in I4B. |
| `TaskStatusPublisher` and reflection launch/host closures | Indirectly; status, query, factory, or host state can retain roots. | Keep externally owned captures as registered roots by I4F.1. A host companion associated only with a managed task is instead **C** and may capture no value. I9/I10 audit the boundary. |
| Runtime input converter and output decoder/callback | Yes, arbitrary host state and public values. | External adapter. Authoritative runtime buffers contain `RuntimeValueRoot` by I4F.1; callbacks run after locks and receive retained public roots. I9 audits retirement. |
| Diagnostic subscribers | Yes, arbitrary host state/public roots. | External root owner converted by I4F.1; subscriptions use weak bus back-references and callbacks run outside locks. I9/I10 audit retirement and containment. |
| Notification/probe `FnOnce` closures | Normally IDs/wakers only; tests may capture fixtures. | Must not become a graph-retention mechanism. Audit constructors in I10. |

## Opaque Payload Families

| Payload | Managed edge status | Decision |
| --- | --- | --- |
| `EffectToken<T>` (`api/value.rs`) | Token contains ID and weak domain only; no core/managed edge. Generic domain payload remains outside the opaque token and may own public roots. | Approved leaf token. A value-bearing domain is an **R** external owner; an edge-free notification/identity domain may be **C**. |
| `ConstructionPort` (`eval/builtins/net/construction.rs`) | Brand and port ID only. | Approved leaf token. |
| `TaskHandleCell` (`reflection/requests.rs`) | Runtime ID, task handle, query handle; no raw core value. Handles reach coordinator/store obligations, not a managed pointer. | Approved external capability; re-audit in I9. |
| `CompilationOrigin` (`diagnostic.rs`) | **Contains raw `core::Value`.** | **D:** replace with exact same-runtime public root or non-value provenance before I4B/G2. |
| Arbitrary host `OpaqueValue::new<T>` | Unknown by type erasure. | Public construction remains restricted to edge-free data or audited same-runtime external roots. I10B.0 decides whether the bootstrap also adds a distinct sealed managed arm; it never permits a bare managed edge inside this `Any` payload. A family with active cleanup remains an external rooted owner with idempotent retirement/RAII under I9F. |

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
   buses are external Rust owners whose retained values become registered roots
   in I4F.1, before the production managed-root switch. They are not interior
   managed pointers; updating them changes root membership rather than invoking
   an object-field mutation gateway. I9 audits their lifecycle but is never the
   first root-conversion phase. Any companion state split from an individual
   managed node is classified separately as **C** and has no such entry.
7. No trace visitor may call user code, allocate, lock unrelated components,
   or force a lazy/promise/net value. Visitors enumerate representation edges
   only.

## Semantic Verification Matrix

These tests latch the requested pre-migration behavior. They are deliberately
kept with the subsystem whose contract they exercise.

| Contract | Existing regression coverage |
| --- | --- |
| Pre-migration public clone/equality baseline and WHNF witness, retained as the compatibility oracle until I4F.2 | `api::tests::evaluated_values_preserve_whnf_identity_and_scalar_views`; `value_evaluator_returns_a_runtime_rooted_whnf_witness`. |
| I2 selected opaque inline-or-managed handle, weak provenance, transport-only traits, runtime-authorized observation, owned extraction, nested scoped access, and complete compatibility-access accounting | the `api::value::prototype::prototype_*` suite; `public_value_compatibility_access_inventory_is_complete`. |
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

The following findings intentionally block collection rather than receiving a
guessing/conservative classification:

1. `CompilationOrigin` hides a raw core value in `OpaqueValue`.
2. `DeferredComputation` and several type-erased compiler/host closures can
   capture raw recursive values. I3E.1 first separates callback-free semantic
   thunks from deterministic external demands; I4B/I10 then reconcile their
   concrete captures.
3. Before I4F.1, `RuntimeValueCache.extensions` hides `GCompilerValues` and
   future arbitrary attachments behind `Any`. I4F.1 must close this registration
   boundary before any production value contains a managed edge; I10 only
   re-audits the already-constrained boundary.
4. Parser/compiler/macro intermediate structures carry raw core values across
   potentially effectful work and need explicit I3 mutator bounds.
5. RPDS and FingerTree/list nodes need reviewed exact logical visitors.
6. I8 must replace the production core `SharedRuntimeNet` `Arc` owner with one
   managed outer cell, then close its exact synchronized trace, durable-handle,
   and value-installing mutation-gateway inventories.
7. Public opaque construction needs the I10B.0 representation decision and a
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
