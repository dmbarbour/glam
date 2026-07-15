# Interaction-net spike

## Shared runtime and lazy copies

`core_net` lowers an explicit function arity and body through an immutable
checked template, then instantiates one `SharedRuntimeNet<CoreSpecialization>`.
Capture locals become leading binds and are supplied by the enclosing semantic
expression. Runtime instantiation preserves the exposed port behind a stable
evaluator-only interface anchor.

A logical copy is target-owned state selected by `CopyId`. Its
`RemoteCursor { copy, remote }` nodes are one-way suspended wires: `remote`
identifies the source interface or an auxiliary port of a source node already
materialized into that copy. A source node is copied only when its principal
faces the cursor. Its auxiliaries become new cursors, while the canonical source
node remains in place and cannot enter a later source-local active pair.

When a demanded cursor faces a source auxiliary whose node belongs to an active
pair, the cursor records that pair's lower-node key. The evaluator reduces only
that exact source pair and then retries the cursor; unrelated source work stays
lazy. If the node is inactive, the cursor instead depends on the target-local
cursor facing its principal. No node and no active pair is copied through an
auxiliary frontier.

Variable use is normalized during lowering:

- zero uses become `Erase`
- one use passes through a builder-only tunnel that finalization splices into a
  direct wire
- multiple uses become a balanced tree of binary `Fan` nodes

`interaction_net` is generic over embedded data and has no dependency on core.
`core_net` owns `CoreNetData` and the `Expr` lowering adapter.

## Checked construction

`NetBuilder` is the construction representation for both compiler lowering and
eventual replay of `interaction_net` effect operations; there is no second
construction IR. Its semantic helpers construct `.bind`, `.data`, and balanced
`.copy N` shapes, while `.wire` connects their ports. `try_wire` and
`try_finish` report invalid, duplicate, exposed, or incomplete wiring without
panicking. The infallible methods remain conveniences for trusted internal
lowering. Builder-only copy tunnels are removed by finalization and never enter
an immutable template or runtime net. Checkpoints and rollback are deliberately
deferred because the source effect can return a write-only operation list for
later replay.

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

Every principal-principal connection is named by an `ActivePairKey` containing
its lower `NodeId`; the partner is recovered from the principal neighbor. One
ordered tree retains every active pair and records whether it is ready, claimed,
blocked on a cursor dependency, or stuck. Claiming is an in-place state change
under the runtime mutex. Completion and targeted source progress are exact
lookups rather than queue scans. Interaction rules, especially erasure,
explicitly remove nodes; there is no separate reachability collector.

## Remaining evaluator bridge

`Value::Net` represents one closed net solely by its shared runtime; it stores
no immutable template, capture environment, or lambda body. Nets compose by
attaching exposed ports through remote cursors. Because an exposed computation
may produce ordinary data rather than a bind, attachment is not intrinsically
a call. `CompileContext::value_net` provides checked construction for Rust
front ends and drops the immutable template after instantiation.

CompileContext lowers a complete source function directly by arity. A source
lambda such as `\x y z -> ...` becomes `FunctionCode` containing one runtime net
with three argument binds (plus any leading capture binds), rather than a
semantic lambda spine. Evaluating its explicit captures produces an observable
`Value::Function`. Partial application derives another shared curried runtime
stage; core has no lambda or closure representation.

Remote cursors remain strictly outward, from a source net into a logical copy;
an inner net cannot retain a cursor back to an outer capture. A logical copy of
a partially applied net can nevertheless encounter a remote cursor already
present in its intermediate source. The outer cursor records the exact source
runtime and intermediate cursor as its dependency. An auxiliary blocked by a
source active pair records that exact pair key instead. The evaluator drives the
specific cursor or pair transitively and retries the outer copy, without a broad
source sweep. This is cursor composition along copy provenance, not reversed
dataflow. Cursor progress claims its target pair before releasing the target
mutex, inspects the source frontier under only the source mutex, and then
finishes under only the target mutex.

Each logical copy keeps an authoritative `frontiers` reverse map from stable
source ports to live local cursors plus fan-site translation. Data is ordinary
`Clone` data; copies do not transform it, and there is no historical source-node
to target-node map. Erase has no cursor-specific shortcut: an active
`Erase >< RemoteCursor` pair demands the cursor, materializes normally, and then
uses the ordinary Erase interaction. If an auxiliary-side cursor has no local
cursor facing the relevant principal yet, source inspection follows that
principal chain to an exact active pair rather than scanning scheduler queues.

`Operator<S::Operator>` is a unary runtime agent whose principal consumes Data
and whose auxiliary is its result continuation. `NetSpecialization` owns both
callable-data interpretation and operator execution. Specialization code runs
outside the net mutex while the active pair is claimed. Success emits Data or a
new Operator automatically wrapped behind a Bind; failure retains the intact
pair and diagnostic in the pair's stuck state. Core uses inspectable
`CoreOperator` enum values rather than opaque Rust closures. `Error` lowering
uses a dedicated operator whose only activated outcome is a stuck pair.

Application spines use the dual construction. `NetBuilder::bind_spine` is
shared by function lowering and evaluator-owned caller nets. `g_syntax` lowers
a maximal application such as `f x y z` through
`CompileContext::value_apply_many`. Ordinary semantic application becomes a
data-consuming operator chain with closed lazy operands. A `Value::Function`
attaches all currently supplied arguments to its shared stage together;
undersaturation returns another shared function stage and saturation returns a
memoized call thunk. Raw `Value::Net` remains the explicit cursor-callable path.

The topology reducer handles bind-bind, fan-fan, fan-bind, fan-data, and eraser
interactions. `bind-data` reports `ReductionKind::Call`; `eval` claims that exact
pair and lowers only its callable data outside the runtime lock. A net
loads through a cursor, while a builtin head becomes Bind/Operator and proceeds
through the ordinary bind join. Dictionary applicables become the same
Bind/Operator shape using an applicable operator. No path
inspects or detaches the argument. Source `Data >< Bind` calls remain exact
source dependencies and are never copied.

Core thunks can be backed by an expression, builtin/access computation, a
closed arity-zero runtime computation, or a saturated function call. All forms
share one memoized result. Saturated calls emit a semantic thunk so unrelated
source-pair progress does not force strict work before its result is demanded.
List, dictionary, and Access lowering use operator chains and store closed lazy
members as ordinary values rather than exporting runtime/interface wires as
aggregate holes.

Only raw `Value::Net` uses callable-data cursor application. Ordinary functions
remain independently observable host values backed by shared curried stages;
their semantic operator application never reifies a linear `Bind`. Do not expose
the `interaction_net` source keyword until general construction effects are
represented explicitly.
The dictionary compatibility path is intentionally unchanged pending a
separate persistent lazy dictionary design.
