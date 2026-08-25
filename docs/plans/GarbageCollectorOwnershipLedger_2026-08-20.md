# Glam GC Ownership and Mutation Ledger — 2026-08-20

Status: Phase I0 complete for the pre-GC representation. Stable integration
facts are reconciled when each representation family receives its concrete
managed wrapper and trace implementation. Collector-private class topology is
verified inside `glam-gc` and is not part of this ledger. Every applicable
family record must be complete before Gate G2 permits production collection.

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
  owner. Its lifetime must be enclosed by a mutator region before managed
  pointers replace its values.
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
5. whether the payload requires `Drop`, its ordinary/finalizing destruction
   policy, and the relevant failure test;
6. every post-publication mutation gateway, or an explicit immutable policy;
7. its external-root classification and the source inventory proving that no
   internal edge was hidden behind a root; and
8. the migration phase and exact verification which authorizes collection.

Final aligned stride, slots per run, dense class identity, metadata address,
frontier state, and other derived run geometry remain collector-internal.
`glam-gc` layout/class tests verify them from public Rust layout and requested
extent inputs. A test-only diagnostic may report them for profiling, but Gate
G2 never depends on copying those instance-specific results into this ledger.
No family may enter production collection while its stable record or visitor
is unresolved.

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
| `SharedRuntimeNet`, `SharedRuntimeNetInner`, `SharedRuntimeNetState` | 8/8, 256/8, 224/8 | Synchronized net owner; final managed boundary chosen in I8. |
| `RuntimeNet`, `RuntimeEntry`, `RuntimeNode` | 200/8, 120/8, 96/8 | Freely mutable only under the shared-net mutex. |
| `CopyState`, `ActivePairState` | 104/8, 48/8 | Net-internal mutable state and cross-net source edge. |

The collector supports per-type requested total slot extents before alignment
rounding, so these layouts do not impose a heap-wide size floor. A request is
not additional padding and cannot be smaller than the Rust representation.
Glam's eventual compact-value policy belongs to the value-representation plan.
Every future managed payload must fit one fixed-size typed-run slot; no current
row is approved for a large-object or multi-run exception.

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
| `Function` | `FunctionValue -> NetValue -> CoreRuntimeNet`; the net carries data/operator values. | Immutable shell over synchronized shared net; long-lived. | Visit the net boundary selected in I8; I6/I8. |
| `Net` | `NetValue -> CoreRuntimeNet`. | Immutable shell over freely mutable net state protected by that net's mutex. | Exact synchronized net visitor and managed-edge gateways; I8. |
| `Lazy` | `Arc<LazyCell>`; source and terminal result graphs. | Identity-bearing, thread-safe, long-lived; source/result publication races are supported. | Managed cell visitor; one-write result plus replaceable source protocol; I5. |
| `Promised` | `Arc<PromiseCell>`; successful assignment root and producer obligation. | Identity-bearing, thread-safe, long-lived; assignment is one-write. | Managed cell visitor; assignment mutation gateway; I5. |
| `Metadata` | `MetadataCarrier.metadata: Arc<Value>`. | Immutable identity-bearing sealed value, thread-safe. | Visit exactly one metadata value; I6. |
| `Opaque` | `Arc<dyn Any + Send + Sync>`; payload-dependent. | Pointer-identity shell; arbitrary longevity/thread transfer. | No conservative trace. Each family must be leaf or hold an exact same-runtime public root; I4B/I10. |

## Recursive Core Nodes

| Type and source | Outgoing edges | Mutation and synchronization | Drop / trace / mutation gateway / phase |
| --- | --- | --- | --- |
| `EvaluationFailure` (`core.rs`) | Emission `Value` or cycle IDs; `Arc<[Value]>` contexts. | Immutable, thread-safe, retained in task/report ledgers. | Visit emission and contexts; ordinary Rust drop initially; I6. |
| `LazyCell` / `LazySource` (`core.rs`) | Sources include fixpoint, deferred closure, reflection computation, access arguments, application, builtin call, net construction/computation, and function call. Terminal `LazyResult` contains evaluated value or failure. | `source` is replaceable only under its mutex; `result` is one-write and published before source removal. Lock order is result check, source mutex, result recheck; destruction occurs after unlock. | Exact source/result visitor. Initial construction needs no gateway; result publication uses one. Deferred/opaque captures block I5 until I4B. Managed drop required because source/result own Rust resources. |
| `PromiseCell` (`core.rs`) | One successful `RuntimeValueRoot`; failure; weak/coordinator producer state and subscriptions. | Assignment and producer are one-write. Coordinator mutation admission encloses task-owned publication; resolver publication uses its local path. Notifications occur after guarded publication. | Move the assignment into a traced managed edge rather than preserving its current registered-root representation. Any separated producer/subscription companion is **C** and edge-free. Assignment gateway and managed drop/finalization policy for unresolved producer ownership; I5. |
| `MetadataCarrier` (`core.rs`) | One `Value`. | Immutable after construction. | Visit one edge; no post-construction gateway; I6. |
| `BuiltinCall`, `Access`, `FunctionCall`, `LazyApplication` (`core.rs`) | Supplied function, arguments, and/or path leaf data. | Immutable `Arc` payloads; thread-safe. | Exact ordered visitors; I6. |
| `FixpointComputation` (`core.rs`) | Function or object-instance `Value`. | Immutable. | Visit one value; I6. |
| `ReflectionComputation` and gate target (`core.rs`) | Effect and gate target values; installed task result may carry a failure. | Effect/target immutable; task one-write. Cross-thread and may outlive original demand. | Visit values and failure; task-result gateway if the node is managed; I6. |
| `DeferredComputation` (`core.rs`) | Type-erased `Arc<dyn Fn(&EvalContext) -> ...>` may capture arbitrary raw core values. | Immutable closure, callable across workers. | **D:** cannot be traced. Contain captures behind exact nodes/roots or replace production closures; I4B/I10. |
| `FunctionCode`, `FunctionValue`, `NetValue` (`core.rs`) | Shared net plus scalar arity/capture state. | Immutable shells, synchronized net interior. | Net visitor in I8; I6. |
| `CoreOperator` variants (`core_net.rs`) | Supplied values, function code, builtin call, applicable value; keys/path elements are leaves. | Immutable operator payloads stored in mutable nets. | Variant visitor; insertion/replacement gateways at net mutation sites; I8. |
| `CoreValues`, `RuntimeValueCache`, extension map (`core.rs`) | Cached singleton values and type-erased `Arc<T>` compiler caches such as `GCompilerValues`. | Runtime-long-lived; core set initialized once, extension map replaceable under mutex, harmless duplicate construction permitted. | Exact registered roots or managed runtime cache. Type-erased extensions are **D** until constrained; I9/I10. |

## Persistent Collections

`List`, `ListNode`, `ListChunk`, `SharedSlice`, `FingerList`, and
`RedBlackTreeMapSync<Key, Value>` are immutable persistent structures. Their
shared spines are ordinary Rust `Arc` ownership today. The initial collector
will trace them logically through public/internal item visitors, even if this
revisits a shared spine more than once. No element insertion occurs after a
node is published, so no post-construction mutation gateway is required unless
I7 later chooses managed mutable spines. `ListThunk::{Lazy, Promised}` visits its one
deferred cell. Bytes chunks and empty nodes are leaves.

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
| `SharedRuntimeNetInner` / `SharedRuntimeNetState` | Own mutable `RuntimeNet`, revision/subscriber/normalization state. | One net mutex is the exclusive graph mutation boundary. No callback or destruction while locked. I8 chooses whether the outer allocation is managed or remains synchronized external storage with exact rooted contents. |
| `RuntimeNet`, `RuntimeEntry`, `RuntimeNode` | Data/operator values, ports, active-pair and cursor state. | Free mutation only under the owning net mutex. Every value-installing rewrite requires the I8 mutation gateway. Exact visitor runs under stopped mutators and the reviewed net-lock protocol. |
| `CopyState`, `CursorClaim`, `PreparedCopySource`, `FrontierObservation` | `SharedRuntimeNet` source references and cursor topology. | Cross-net references are hierarchical copies, but value-level fixpoints/promises can still make the surrounding value graph cyclic. Visit source net exactly if it becomes managed; I8. |
| `NormalizationRequest`, `RequestRoot`, `Cursor`, `ActivePair`, `ResumeCursorDependency`, `NetDriverWorklist` (`eval/net.rs`) | Temporary/shared runtime-net handles. | Evaluator/work-quantum owners, movable between workers; T until enclosed by I3 mutator scope. |

## Registered Root and Transient Owner Inventory

The following named types are exhaustive for source-visible fields containing
`core::Value`, public `Value`, `RuntimeValueRoot`, `EvaluatedValue`, a store
snapshot, or a diagnostic which owns those values as of this ledger. A grouped
row shares one ownership policy; every listed type remains individually in the
inventory.

| Area | Types | Classification, lifetime, and migration |
| --- | --- | --- |
| Runtime value domain | `RuntimeValueDomain`, `CoreValueFactory`, `Values`, `RuntimeValueFactory`, `RuntimeSharedResources` | Authorized strong value-domain leases under I1B. The domain owns the no-auto heap, IDs, cache, and weak coordinator binding; it is never managed by its own heap. Cached payloads cannot retain a factory/domain backedge. Cache roots and retained service factories are audited in I9/I10. |
| Public root facade | `RuntimeValueRoot`, `api::Value`, `EvaluatedValue`, `PromiseResolver`, `EffectTokenDomain`, `EffectTokenDomainState` | R. Public/runtime-long-lived and cross-thread, but non-owning with respect to the value domain. Root registration/provenance and inert access are selected in I2; all root surfaces are audited in I9. Resolver ownership/finalization is handled with promises in I5; generic effect-token domain payloads remain explicit external root owners. Any promise coordination split from its managed cell is separately **C** and contains no value root. |
| Assembly facade | `AssemblerReflectionHost`, `CompilationExecution`, `ReasoningSession`, `CompileSetup`, `BuiltModule`, `ReasoningVolume`, `Assembler`, `DiagnosticAttachment`, `AssemblerBuilder`, `ModuleBuilder` | R for stored public roots/diagnostics; T for build setup raw core values. Convert setup/compiler fields to scoped managed access in I3/I9. |
| Diagnostics | `Diagnostic`, `DiagnosticEvent`, `DiagnosticBusState`, `DiagnosticBusInner`, `DiagnosticBus`, `DiagnosticIngressInner`, `DiagnosticIngress`, `DiagnosticSubscription`, `DiagnosticSubscriptionInner`, `Error`, `ReasoningFailure` | R. Buses/callbacks are external root owners; events retain public roots until delivery/retirement. Weak back-references remain non-owning. I9. |
| Runtime events | `RuntimeInputRecord`, `RuntimeOutputIntent`, `RuntimeDeliveryRecord`, `RuntimeEventSnapshot`, `RuntimeEventJournal`, `RuntimePreparedInput`, `RuntimeDeliveryTicket`, `RuntimeDiagnosticRoute` | R. Persistent input snapshots and identified deliveries retain roots across threads and settlement. I9. |
| Readiness/reporting | `QuiescenceSnapshot`, `QuiescenceReport`, `DeadlockSnapshot`, `RuntimeDeadlockWork`, `EvaluationSessionReport`, `EvaluationUnfinishedTask` | R. Host-visible snapshots may outlive sessions/runtime facade. Failures and store snapshots remain exact roots; I9. |
| Reflection store/protocol | `State`, `Set`, `Rewrite`, `StoreSnapshot`, `StoreJournal`, `ReflectionStore`, `Scoped`, `HostSnapshot`, `TaskCommit`, `Transaction`, `ReflectionJournal`, `QueryRead` | R. Store roots are persistent public roots; `ReflectionStore` and `StoreSnapshot` also retain an authorized factory/domain lease while they can construct transaction/query values. Journals are transaction-local snapshots/edits; store mutation uses runtime mutation admission then store/event mutex, with wakes/destruction after unlock. I9. |
| Reflection lifecycle/search | `EffectRun`, `IsolatedTaskHost`, `IsolatedSearchBranch`, `IsolatedSearchBlock`, `IsolatedEffectSearch` | R/T. Runs and isolated search retain public roots while active; same-runtime only. I3/I9. |
| Reflection machine | `EffectTask`, `ContextualValueEffectTask`, `Branch`, `Deliver`, `Apply`, `CutFrame`, `TaskBlock`, `FixRoot`, `ActiveFix`, `Restore`, `ResetFrame` | T transitioning to exact machine-owned roots. One claimed machine is exclusively mutable outside coordinator locks and may move between workers; any parked value must be rooted. I3/I9. |
| Evaluation coordinator | `EvaluationDemandState`, `SettlementObligations`, `TaskOwnedPromiseObligation`, `DeferredLazyCycleMember`, `DeferredWorkRelease`, `RuntimeSettlementRelease`, `SparkDemand`, `EvaluationTaskBlock`, `PromiseProducerObligation`, `LocalPromiseObligation`, `LocalPromiseOwner`, `PendingReflectionTaskInner` | R/T. `EvaluationDemandState` retains an authorized factory/domain lease; the coordinator route back from that domain is weak. Coordinator state owns parked work and registered roots; weak promise cells are non-owning. State changes use mutation admission plus one component mutex; callbacks/drop happen after unlock. I3/I5/I9. |
| Evaluator machines | `LazyTaskMachine`, `LazyTaskWork`, `PromiseFollower`, `PromiseFollowerState`, annotation builtin state (`AssertUnit`, `MetadataPure`, `MetadataReflection`, `Reflection`, `Seq`, `Spark`, `Context`, `Valid`), net-construction `Data`/`NetConstructionMachine`, pattern `Found` | T. Bounded poll/quantum state, except parked machines owned by coordinator. Explicit mutator propagation in I3; managed edges in I5/I6/I8. |
| Compiler API | `ModuleLoadArgs`, `CompileContext`, `CompileDiagnosticEmitter`, `ModuleLoader`, `BinaryFileLoader` | T/D. Compile calls are bounded, but raw core values and value-capturing callbacks require mutator scope and capture containment. I3/I4B/I10. |
| `.g` compiler cache | `BuiltinModule`, `GCompilerValues` | R/D. Runtime extension cache, long-lived and cross-thread; raw core values hidden behind `Any` block collection until converted to exact runtime roots/cache nodes. I9/I10. |
| `.g` lowering/resolution | `LoweredSource`, `g_syntax::Diagnostic`, `MacroSnapshot`, `MacroJournal`, `MacroRun`, `MacroFailure`, `ModuleLowerer`, `ResolvedNetLowerer`, `ResolvedDoBlock`, `EffectBind`, `ValueBind`, `Then`, `ResolvedBindings` | T. Compilation/macro-run scoped values; mutator scope in I3 and no parking unrooted across evaluation. |
| `.g` lexical/parser | `LexedSource`, `Lexer`, `DeclarationMacroWork`, `StagedSourceParser`, `ParsedSource`, `InspectedSource`, `ParseSession` | T. Embedded source data and diagnostics are bounded by source compilation, but macro evaluation can suspend; I3. |
| CLI/configuration binary | `CliCaseExplanation`, `CliCompletion`, `ExpectationEvidence`, `CliJournal`, `CliSearchResult`, `SuccessfulBranch`, `TokenRun`, `CliError`, `CliExpansion`, `MainJournal`, `LoggerTaskHost`, `LoggerRun`, `LogHost`, `LoggerSupervisor`, `PreparedAssembly`, `LoadedConfiguration`, `DefaultLogger` | R/T outside the library crate. Public roots and diagnostics are retained through runtime callbacks/sessions. They require only public-root discipline plus bounded evaluation entries, I3/I9. |
| Public net builder | `NetBind`, `NetCopy`, `NetBuilder` | T. Borrowed ports and a construction-local core net builder; cannot cross completion of construction. I3/I8. |

## Type-Erased Closure and Callback Inventory

| Family | Can capture values? | Boundary decision |
| --- | --- | --- |
| `DeferredComputation` | Yes, raw `core::Value` today. | D. Replace/contain in I4B/I10; never conservatively trace a closure environment. |
| `ModuleLoader`, `BinaryFileLoader`, `CompileDiagnosticEmitter` | Yes; supplied by compiler setup and may capture core/public values. | Compilation-scoped T, but any suspension/caching requires exact same-runtime roots. Audit concrete constructors in I4B. |
| `TaskStatusPublisher` and reflection launch/host closures | Indirectly; status, query, factory, or host state can retain roots. | Keep externally owned captures as registered roots. A host companion associated only with a managed task is instead **C** and may capture no value. I9/I10. |
| Runtime input converter and output decoder/callback | Yes, arbitrary host state and public values. | External adapter. Authoritative runtime buffers contain `RuntimeValueRoot`; callbacks run after locks and receive retained public roots. I9. |
| Diagnostic subscribers | Yes, arbitrary host state/public roots. | External root owner; subscriptions use weak bus back-references and callbacks run outside locks. I9/I10. |
| Notification/probe `FnOnce` closures | Normally IDs/wakers only; tests may capture fixtures. | Must not become a graph-retention mechanism. Audit constructors in I10. |

## Opaque Payload Families

| Payload | Managed edge status | Decision |
| --- | --- | --- |
| `EffectToken<T>` (`api/value.rs`) | Token contains ID and weak domain only; no core/managed edge. Generic domain payload remains outside the opaque token and may own public roots. | Approved leaf token. A value-bearing domain is an **R** external owner; an edge-free notification/identity domain may be **C**. |
| `ConstructionPort` (`eval/builtins/net/construction.rs`) | Brand and port ID only. | Approved leaf token. |
| `TaskHandleCell` (`reflection/requests.rs`) | Runtime ID, task handle, query handle; no raw core value. Handles reach coordinator/store obligations, not a managed pointer. | Approved external capability; re-audit in I9. |
| `CompilationOrigin` (`diagnostic.rs`) | **Contains raw `core::Value`.** | **D:** replace with exact same-runtime public root or non-value provenance before I4B/G2. |
| Arbitrary host `OpaqueValue::new<T>` | Unknown by type erasure. | Public construction must remain restricted. Production families need a sealed registration declaring leaf or exact public-root fields; I4B/I10. |

An opaque value may not contain `Gc<T>`, an unrooted recursive core value, or a
root belonging to another runtime. The collector will not inspect `Any` or a
closure environment for hidden pointers.

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
   buses are external Rust owners where their entries are registered roots,
   not interior managed pointers; updating them changes root membership rather
   than invoking an object-field mutation gateway. Any companion state split
   from an individual managed node is classified separately as **C** and has no
   such entry.
7. No trace visitor may call user code, allocate, lock unrelated components,
   or force a lazy/promise/net value. Visitors enumerate representation edges
   only.

## Semantic Verification Matrix

These tests latch the requested pre-migration behavior. They are deliberately
kept with the subsystem whose contract they exercise.

| Contract | Existing regression coverage |
| --- | --- |
| Public clone/equality and WHNF witness | `api::tests::evaluated_values_preserve_whnf_identity_and_scalar_views`; `value_evaluator_returns_a_runtime_rooted_whnf_witness`. |
| Cross-runtime rejection | `public_value_factories_reject_foreign_composite_members`; `assembler_boundaries_reject_foreign_values_before_evaluation_or_storage`; `runtime_input_endpoints_are_local_monotonic_capabilities`. |
| Fulfilled/unfulfilled lazy and resolver promise | `value_evaluator_caches_lazy_success_and_preserves_structured_failure`; `value_evaluator_resumes_a_retained_resolver_promise_subscription`; `promised_assignments_retain_deferred_aliases`. |
| Pure lazy cycle | `a_lazy_task_that_waits_on_itself_is_poisoned_as_a_cycle`; `concurrently_demanded_lazy_tasks_share_one_two_node_cycle_failure`; `two_sessions_share_and_retire_one_pure_lazy_cycle_failure`. |
| Promise/lazy cycle | `promise_only_cycle_remains_blocked_without_poisoning_its_assignment`; `mixed_promise_lazy_cycle_remains_retryable_without_poisoning_the_lazy`; `a_cross_session_promise_lazy_cycle_remains_unpoisoned`. |
| Metadata/collection deferred cycle and graph identity | `core::tests::metadata_and_collections_can_participate_in_a_deferred_value_cycle`; `metadata_carriers_hide_unit_and_associated_metadata`; metadata update reorder/copy tests in `eval::tests`. |
| Function/collection/net recursive graph | `evaluates_recursive_dictionary_net`; `compiled_function_values_reuse_one_shared_interaction_net`; `curried_function_partial_application_retains_a_shared_stage`; net cursor/copy runtime tests. |
| Same-runtime worker transfer | `workers_force_sparks_and_poll_ready_reflection_tasks` and the evaluation executor worker tests. |
| Runtime facade drop with escaped value/resources | `value_evaluator_returns_a_runtime_rooted_whnf_witness`; `runtime_shared_resources_do_not_retain_runtime_lifecycle_owners`; `public_values_retain_only_the_runtime_value_domain`; `bare_public_values_do_not_retain_the_runtime_value_domain`; `evaluation_context_retains_runtime_cache_and_profile_without_a_cycle`; `retained_reflection_profile_keeps_only_shared_resources_alive`; `compiler_cache_does_not_form_a_value_domain_cycle`. |
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
   capture raw recursive values.
3. `RuntimeValueCache.extensions` hides `GCompilerValues` and future arbitrary
   attachments behind `Any`.
4. Parser/compiler/macro intermediate structures carry raw core values across
   potentially effectful work and need explicit I3 mutator bounds.
5. RPDS and FingerTree/list nodes need reviewed exact logical visitors.
6. `SharedRuntimeNet` needs an exact synchronized trace and complete
   value-installing mutation-gateway inventory.
7. Public opaque construction needs a closed leaf/root registration boundary.

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
