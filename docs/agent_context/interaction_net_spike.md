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

## Pairing oracle

Fan interaction must distinguish paired residuals of one duplication process
from independent fans. Do not regress this to equality of a static site or a
flat process-global UID. `RuntimeNet::reduce_next_with` asks a `FanOracle`.

`HistoryOracle` currently gives each fan a persistent duplication context. Fan
commutation extends the context with the fan crossed and the selected branch;
identical complete identities annihilate and other identities commute. This is
a correctness-oriented, non-local representation of the oracle, not a claim to
have implemented Lamping's optimal bookkeeping.

The intended next representation replaces explicit histories with local
bracket/croissant control nodes that encode the same enclosure transitions.
Keep `FanOracle` as the comparison seam while validating that local encoding.

## Runtime identity and scheduling

Runtime node and pair IDs are monotonically allocated `u64` values stored in
hash tables and never reused. A `Port` packs its node ID and two-bit port index
into one nonzero word, so `Port` and `Option<Port>` are both one word. Node
records keep all three possible links inline.

Every principal-principal connection owns an explicit pair record. Ready pairs
are queued, unresolved calls transition to blocked, and data-data type errors
transition to stuck. Cancelled queue IDs cannot alias later pairs because pair
IDs are monotonic. Interaction rules, especially erasure, explicitly remove
nodes; there is no separate reachability collector in this representation.

## Remaining evaluator bridge

The topology reducer handles bind-bind, fan-fan, fan-bind, fan-data, and eraser
interactions. `bind-data` still reports `Reduction::Call`; ordinary assembly
evaluation uses the prior call-by-need evaluator through `Closure::source_body`.
Do not expose the `interaction_net` source keyword until calls and observation
run demand-first through runtime nets.
