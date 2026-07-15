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

Every principal-principal connection appears in exactly one scheduler
collection. New pairs enter the ready queue; unresolved bind calls, pending or
blocked host calls, and remote cursors move to their respective queues; and
no-rule pairs or permanent host errors move to the diagnostic-bearing stuck
list. Reduction results retain the originating pair and calls identify the node
roles needed for later completion. Interaction rules, especially erasure,
explicitly remove nodes; there is no separate reachability collector.

## Remaining evaluator bridge

`Value::Net` represents one closed net solely by its shared runtime; it stores
no immutable template, capture environment, or lambda body. Nets compose by
attaching exposed ports through remote cursors. Because an exposed computation
may produce ordinary data rather than a bind, attachment is not intrinsically
a call. `CompileContext::value_net` provides checked construction for Rust
front ends and drops the immutable template after instantiation.

During migration, CompileContext prepares closed curried lambda spines with no
captures, nested function values, or dictionary access. A source lambda such as
`\x y z -> ...` is constructed in one compiler call and lowers to one runtime
net containing three leading binds, rather than preparing a net per semantic
lambda wrapper. Partial application exposes the next bind in that same net.
Other lambdas retain the compatibility closure path.

Remote cursors remain strictly outward, from a source net into a logical copy;
an inner net cannot retain a cursor back to an outer capture. A logical copy of
a partially applied net can nevertheless encounter a remote cursor already
present in its intermediate source. The outer cursor records the exact source
runtime and intermediate cursor as its dependency. The evaluator releases the
outer lock, drives that cursor transitively, and retries the outer copy. This is
cursor composition along copy provenance, not reversed dataflow, and avoids
holding nested runtime locks. Runtime calls also defer capturing lazy arguments
while the runtime still exposes an unsupplied bind from its curried spine.

`HostFn<Data>` is a unary runtime agent whose principal consumes Data and whose
auxiliary is its result continuation. Host callbacks execute outside the net
mutex. Success emits Data or a new HostFn automatically wrapped behind a Bind;
retryable blocks keep their active pair in a blocked queue, while permanent
errors retain the intact pair and diagnostic in the stuck collection. Core
builtin expressions lowered into nets use this path, although saturated work
remains a memoized semantic thunk and direct evaluator builtin values retain a
compatibility path.

Application spines use the dual construction. `NetBuilder::bind_spine` is
shared by lambda lowering and evaluator-owned caller nets. `g_syntax` lowers a
maximal application such as `f x y z` through
`CompileContext::value_apply_many`; the evaluator peels the left-associated
semantic `Apply` nodes and, for a net or net-evaluable closure, installs all
remaining lazy arguments into one caller runtime. Compatibility-only callables
remain sequential, and a partial application that escapes its expression still
uses cursor composition when it is called later.

The topology reducer handles bind-bind, fan-fan, fan-bind, fan-data, and eraser
interactions. `bind-data` reports `ReductionKind::Call`; `eval` consumes that
blocked pair through a generic `CallFrame`, preserving the argument and result
wires behind stable interfaces. A runtime remembers whether it imported a
logical copy, because only an instance may detach a lazy argument wire. Doing
that in the canonical lambda runtime would capture its unsupplied root.

Core thunks can be backed by an expression, a runtime/interface pair, or a
semantic builtin/access/list-item computation. All forms share one memoized
result. Builtins are callable `CoreNetData`, partial applications retain shared
thunks, and saturated calls emit a semantic thunk so conservative source sweeps
do not force strict work before its result is demanded. List lowering similarly
retains computed elements as opaque lazy list holes.

Closure application runs through the core runtime driver for lambda bodies made
from values, applications, locals, nested lambdas, lists, deferred values,
futures, and errors. Dictionary access is the remaining compatibility boundary:
a copied access can expose a demanded local through a second logical-copy
boundary, but demand is not yet forwarded from that cursor to the caller-side
frontier. Such closures retain `Closure::source_body` and use the expression
evaluator. Do not expose the `interaction_net` source keyword until that
cross-copy demand edge and general effect blocking are represented explicitly.
The dictionary compatibility path is intentionally unchanged pending a
separate persistent lazy dictionary design.
