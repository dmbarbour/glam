# Interaction-Net Implementation Invariants

This note is the current contract for the generic net implementation, its core
specialization, and the source construction boundary. It is not a chronology
of the interaction-net migration.

## Ownership

- `src/interaction_net/model.rs` owns generic identities, agents, ports, and
  the specialization protocol.
- `src/interaction_net/builder.rs` owns checked immutable construction.
- `src/interaction_net/runtime/` owns mutable graph storage, active-pair
  reduction, cursors, and runtime tests.
- `src/core_net.rs` supplies core `Value` and `CoreOperator` semantics.
- `src/g_syntax/net_lowering.rs` lowers front-end functions and applications.
- `src/eval/builtins/net/construction.rs` interprets source construction
  effects and replays the selected journal.
- `src/eval/net.rs` and `src/eval/operator.rs` drive specialization work.

Keep syntax and core policy out of the generic interaction-net modules.

## Templates and Construction

An `InteractionNet<S>` is an immutable reusable template with one exposed port.
Every other port is wired exactly once, so a net stored as a value is closed at
its sole interface.

The stored agents are:

- `Bind`, with principal application plus argument and result auxiliaries;
- binary `Fan`, with a template-local `FanSite`;
- `Erase`;
- specialization-owned `Data`; and
- specialization-owned unary `Operator`, whose principal consumes data and
  whose auxiliary is the result continuation.

`NetBuilder` is the only construction representation. Its checked wiring and
finalization report foreign ports, duplicate wires, incomplete topology, and
invalid exposure. `copy(0)` emits erasure, `copy(1)` emits a builder-only tunnel
that finalization splices into a direct wire, and larger copies use a balanced
binary fan tree. Tunnels never enter a template or runtime.

`Assembler::net` is a lifetime-scoped, core-specialized facade over this same
builder. Source `interaction_net` also ends at this builder: its branch-local
write-only operation journal is transaction state, not a second graph IR.

`interaction_net Effect` is lazy and memoized. Its isolated freer machine
provides `.bind`, `.copy`, `.data`, and `.wire` together with the standard
task-local effects, but no reflection, shared heap, environment, logging, or
task capabilities. Each invocation brands its opaque logical port handles;
operations reject handles from another invocation. Alternatives cheaply share
a persistent journal prefix. No partial graph is built while searching.

At completion, zero successful branches fail, more than one is ambiguous, and
exactly one must return a branded port to expose. Only that branch is replayed
in order through `NetBuilder`, then instantiated once as the runtime memoized
by the construction lazy. Failed alternatives are never finalized, so their
partial topology cannot produce spurious build errors. `.data` records its
payload without forcing it.

## Runtime Identity and Graph State

- `NodeId` is a zero-based logical ID encoded as `NonZeroU64`. Runtime IDs are
  allocated monotonically, stored in a hash table, and never reused.
- `Port` packs a node ID and two-bit port index into one `NonZeroU64`; a node
  stores its three possible links inline.
- Rewrites remove nodes explicitly. There is no reachability collector: after
  explicit fans and erasers, topology is linear.
- Runtime instantiation adds an evaluator-only `Interface` anchor around the
  template's exposed port. The returned interface port remains stable.

A principal-principal connection is keyed by the lower endpoint's `NodeId`.
Because the partner is the principal neighbor, the key is sufficient to recover 
and validate the full pair.

One ordered active-pair map is authoritative. Each live pair is `Ready`,
`Claimed`, blocked on a cursor, or permanently `Stuck`. Removing a ready pair
from consideration is an implicit claim only while the runtime lock is held;
external work records `Claimed` in place so another worker cannot take it.
Stuck pairs should be exceptional and remain visible for diagnostics rather
than moving to a separate queue.

## Reduction and External Work

Only principal-principal pairs reduce. Ordinary topology rules rewrite under
the runtime mutex. Work delegated to a specialization follows this sequence:

1. claim the exact pair under the mutex;
2. copy out the immutable request data;
3. release the mutex;
4. run callable, operator, or cursor work; and
5. reacquire only the owning runtime long enough to complete, block, or mark
   that same pair stuck.

Do not rediscover work by scanning active-pair collections, remove elements
from the middle of queues, or hold source and target runtime mutexes together.
An `Erase >< RemoteCursor` pair has no shortcut: it demands normal cursor
materialization, after which the ordinary erasure rule applies.

## Logical Copies and Cursors

A logical copy is target-owned `CopyState` containing:

- the shared source runtime;
- a reverse `frontiers` map from stable source ports to live target cursor
  nodes; and
- source-to-target fan-site translation.

There is deliberately no source-node to target-node history. Embedded data is
ordinary `Clone` data. Copied target nodes may reduce or disappear immediately,
so their former source identity is not useful provenance.

`RemoteCursor { copy, remote }` is a target-local, principal-only agent and a
one-way suspended wire from source to target. `remote` identifies the source
interface port or an auxiliary port of a source node already materialized for
that logical copy. Source port IDs remain stable because source nodes are not
moved or rewritten after their principal frontier has been exported.

Cursor progress obeys these rules:

- A cursor matters only when its principal participates in local demand.
- A node materializes only when the cursor's remote neighbor is that node's
  principal port. The node is cloned into the target and each source auxiliary
  becomes a new cursor.
- An active pair never materializes across the boundary. If the frontier enters
  an auxiliary whose source node participates in an active pair, the cursor
  records that exact source `ActivePairKey`; the evaluator reduces it in the
  source and retries.
- If the source node is inactive, dependency inspection follows its principal
  chain to an exact source pair or to another already materialized frontier. It
  does not copy through the auxiliary.
- When two target cursors reach opposite ends of one source wire, the
  `frontiers` reverse map joins them into a local tunnel. A claimed peer is left
  intact until its claim finishes.
- If a source frontier is itself a cursor from an intermediate logical copy,
  the outer cursor records that exact source cursor dependency and drives it
  transitively. Content from the deeper net is not copied directly into the
  outer target.

This preserves one-way dataflow and work sharing: shared source-local active
pairs normalize only in the shared source, while each target receives stable
frontier nodes on demand.

## Fans

`FanSite` is a runtime-local `u64`; there is no process-global instance ID. Each
logical copy translates source sites into fresh target-local sites.

The current `FanIdentity` also stores its complete dynamic duplication context.
Commutation extends that context with the crossed fan and branch. Identical
complete identities annihilate; other fans commute. Static site equality alone
is incorrect.

This history representation is a correctness reference, not Lamping's optimal
bookkeeping. Replacing it with bracket/croissant control agents must replace
identity construction and fan rewrite rules together; an interchangeable
comparison oracle is insufficient.

## Core Specialization

`NetSpecialization` defines cloneable `Data`, cloneable unary `Operator`, a
permanent error, callable-data interpretation, and `Operator >< Data`
execution.

- `Data >< Bind` asks `callable` for either a shared net or an operator. A net
  installs a logical-copy cursor. An operator is placed behind a fresh `Bind`
  so the ordinary bind-join rule performs application. Failure leaves the
  original pair stuck.
- `Operator >< Data` executes outside the runtime lock and yields either `Data`
  or another `Operator`. A returned operator is bind-wrapped for the next
  argument. Failure is permanent; there is no retryable host-blocking state.
- Core uses explicit `CoreOperator` enum values rather than opaque Rust
  closures. `Error` is an operator whose activated result is stuck.

Ordinary `Value::Function` application is evaluator-owned semantic staging and
does not reify a linear bind as a host function. Raw `Value::Net` is opaque
closed data already in WHNF; ordinary `apply_value` must not reinterpret it as
a lambda-calculus callable. When a `Data(Value::Net)` node instead meets a
`Bind` inside an interaction net, `CallableData` installs a logical-copy cursor
at the raw net's exposed interface. This runtime call reduction is the only
implicit operation that opens the net.

`HostFn`, copying, and erasure otherwise treat `Value::Net` like closed data;
they do not project its exposed agent. A net-backed `Value::Lazy` represents
the explicit zero-arity bridge and must produce `Data` when observed.
`FunctionValue` staging is the positive-arity bridge: partial application only
attaches arguments and never inspects the intermediate interface. Saturation
must produce `Data`; an early `Data` is left to ordinary interaction rules and
may become stuck as later arguments are attached. The provisional source form
for both bridges is `net_arity N Net` and is available through `import 'std`.
The same module provides the ordinary `interaction_net` construction builtin;
the bootstrap currently writes its effect programs with explicit `>>=` and
`=>>` while `do` notation remains future syntax.

Shared runtime mutation increments a condition-variable generation. If one
observer encounters an active pair already claimed by another evaluator, it
waits for that exact runtime to change and retries; a claimed pair must never
be misreported as quiescence. Cursor dependencies similarly treat a source
pair disappearing between inspection and claim as progress and refresh their
frontier.

## Deliberate Limits

- Node IDs and fan sites are not recycled.
- The scheduler is correctness-oriented. Configured background workers can
  evaluate sparks and thereby reduce shared nets, but runtime work stealing and
  finer-grained wake indexes are not implemented.
- Direct fan histories remain potentially large.
- Stuck pairs are retained for inspection but reflection does not yet expose
  them.
- Dictionary applicability remains a compatibility path pending a persistent
  lazy dictionary design.
