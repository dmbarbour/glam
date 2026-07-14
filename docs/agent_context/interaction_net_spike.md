# Interaction-net spike

## Template and runtime identity

`core::Lambda` owns one `OnceLock<Arc<CoreInteractionNet>>`. `core_net` lowering
assigns small `FanSite` numbers local to that immutable template.
`InteractionNet::instantiate`
allocates one process-global `InstanceId` and qualifies every fan as
`(InstanceId, FanSite)`, so instantiation does not traverse the graph merely to
allocate a fresh global ID for each fan.

Variable use is normalized during lowering:

- zero uses become `Erase`
- one use becomes a direct wire
- multiple uses become a balanced tree of binary `Fan` nodes

`interaction_net` is generic over embedded data and has no dependency on core.
`core_net` owns `CoreNetData` and the `Expr` lowering adapter.

## Fan history

Fan interaction must distinguish paired residuals of one duplication process
from independent fans. Do not regress this to equality of a static site or a
flat process-global UID.

Each fan currently carries a persistent duplication context. Fan commutation
extends the context with the fan crossed and the selected branch; identical
complete identities annihilate and other identities commute. This is a
correctness-oriented, non-local representation, not a claim to have implemented
Lamping's optimal bookkeeping.

The intended next representation replaces explicit histories with local
bracket/croissant control nodes that encode the same enclosure transitions.
That change must replace identity construction and the relevant rewrite rules
together; it is not modeled as an interchangeable comparison oracle.

## Runtime identity and scheduling

Runtime node IDs are monotonically allocated `u64` values stored in a hash table
and never reused. A `Port` packs its node ID and two-bit port index into one
nonzero word, so `Port` and `Option<Port>` are both one word. Node records keep
all three possible links inline.

Every principal-principal connection appears in exactly one scheduler
collection. New pairs enter the ready queue, unresolved calls move to the
blocked queue, and data-data type errors move to the stuck list. Reduction
results retain the originating pair, and calls identify the bind and data node
roles needed for later completion. Interaction rules, especially erasure,
explicitly remove nodes; there is no separate reachability collector.

## Remaining evaluator bridge

The topology reducer handles bind-bind, fan-fan, fan-bind, fan-data, and eraser
interactions. `bind-data` still reports `ReductionKind::Call`; ordinary assembly
evaluation uses the prior call-by-need evaluator through `Closure::source_body`.
Do not expose the `interaction_net` source keyword until calls and observation
run demand-first through runtime nets.
