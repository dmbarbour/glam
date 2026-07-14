# Interaction-net spike

## Shared runtime and lazy copies

`core::Lambda` owns one once-initialized `SharedRuntimeNet<CoreNetData>`.
`core_net` still lowers through an immutable checked template, but instantiates
that template once; closures for the same lambda share its partially normalized
runtime. Runtime instantiation preserves the exposed port behind a stable
evaluator-only interface anchor.

A logical copy is target-owned state selected by `CopyId`. Its
`RemoteCursor { copy, remote }` nodes are one-way suspended wires: `remote`
identifies the source interface or an auxiliary port of a source node already
materialized into that copy. A source node is copied only when its principal
faces the cursor. Its auxiliaries become new cursors, while the canonical source
node remains in place and cannot enter a later source-local active pair.

When a demanded cursor faces a source auxiliary port, the conservative initial
scheduler reduces each pair that was ready at the beginning of one source
sweep, then retries the cursor. Newly created pairs wait for a later sweep.

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

`FanSite` is a runtime-local `u64`; there is no global `InstanceId`. Every
logical copy has a source-site to target-site translation map. Translating a fan
recursively translates the sites in its complete residual history, preserving
identity relationships inside the copy while keeping independent copies
distinct in one target runtime.

## Runtime identity and scheduling

Runtime node IDs are monotonically allocated `u64` values stored in a hash table
and never reused. A `Port` packs its node ID and two-bit port index into one
nonzero word, so `Port` and `Option<Port>` are both one word. Node records keep
all three possible links inline.

Every principal-principal connection appears in exactly one scheduler
collection. New pairs enter the ready queue, unresolved calls and remote
cursors move to their blocked queues, and data-data type errors move to the
stuck list. Reduction results retain the originating pair, and calls identify
the bind and data node roles needed for later completion. Interaction rules,
especially erasure, explicitly remove nodes; there is no separate reachability
collector.

## Remaining evaluator bridge

The topology reducer handles bind-bind, fan-fan, fan-bind, fan-data, and eraser
interactions. `bind-data` still reports `ReductionKind::Call`; ordinary assembly
evaluation uses the prior call-by-need evaluator through `Closure::source_body`.
Do not expose the `interaction_net` source keyword until calls and observation
run demand-first through runtime nets.
