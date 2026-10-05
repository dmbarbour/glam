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
- `src/eval/builtins/net/runner.rs` interprets source construction effects
  against the pure builder, selects one result, and reaches replay.
- `src/eval/builtins/net/builder.rs` owns the evaluator-private pure
  state-over-list handler, including the fixed-width protected state, compact
  construction transitions, and exact private API.
- `src/eval/builtins/net/identity.rs` owns invocation-local brands and opaque
  logical port handles; its token family is edge-free.
- `src/eval/builtins/net/netlist.rs` validates and replays the selected strict
  semantic netlist.
- `src/eval/net.rs` and `src/eval/operator.rs` drive specialization work.

Keep syntax and core policy out of the generic interaction-net modules.

## Templates and Construction

An `InteractionNet<S>` is an immutable reusable template with one exposed port.
Every other port is wired exactly once, so a net stored as a value is closed at
its sole interface.

The stored agents are:

- `Bind`, whose two auxiliaries have opposite polarity: an application lists
  `[argument, result]` and a function `[result, argument]`. `Bind >< Bind`
  joins them crossed (`1-2`, `2-1`), while identical fans join positionally.
  `NetBuilder::bind` returns the application order. `function_bind` and
  `function_spine` name function ports by role, and `application_spine`
  builds applications;
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
builder. Source `interaction_net` searches with a pure, branch-local builder
state over ordered lazy lists. After selecting one result, it encodes a
strict ordinary-value netlist and passes that to the evaluator-private
`InteractionNetFromNetlist` builtin. That synchronous boundary validates
brands, a derived monotonic port cursor, compact constructor and wire journal
shapes, and topology before lowering through `NetBuilder`; it performs no
demand, effect dispatch, callback, wait, or root retention. Constructor
records omit their derivable ports and wire records contain only logical ID
pairs. The semantic netlist is a checked replay protocol, not a second mutable
graph IR. No dedicated lazy-source construction machine remains.

`interaction_net Effect` is lazy and memoized. Its pure runner applies the
effect's `eff` handler to a private API providing `.bind`, `.copy`, `.data`,
and `.wire` together with the standard task-local effects, but no reflection,
shared heap, environment, logging, or task capabilities. Each invocation
brands its opaque logical port handles; operations reject handles from another
invocation. Alternatives cheaply share persistent builder-state and journal
prefixes. No partial graph is built while searching.

Construction effects follow the same recursive `eff` discipline as
`ListEffect`. The effect operands of `.seq`, `.alt`, `.cut`, `.reset`,
`.shift`, and `.fix` are interpreted against the same private API before they
receive builder state. A captured `.shift` continuation is an ordinary
`Value -> Effect` callable, never itself an effect: applying it yields an `eff`
value for the same runner. It is reusable within its own invocation and
rejected by brand in any other. The hidden reset stack lives under a private
key inside user state, so `.get []` and `.set []` capture, clear, and restore
it. The active sequence/cut stack is a separate protected builder field, so
`.set []` cannot erase the sequence that is executing it.

At completion, zero successful branches fail, more than one is ambiguous, and
exactly one must return a branded port to expose. Only that result is encoded
and replayed in order through the hidden semantic-netlist boundary, then
instantiated once as the runtime memoized by the construction lazy. Failed
alternatives are never finalized, so their partial topology cannot produce
spurious build errors. `.data` records and replays its payload without forcing
it.

Search order is authoritative:

- Outcomes are observed in source order. A blocked left alternative blocks the
  whole search; a ready right alternative is never selected ahead of it.
  `.cut` selects the first success in that order.
- Selection observes at most two outcomes, one list front at a time. The
  second-result check stays lazy; if it blocks, selection blocks rather than
  accepting the first.
- Ambiguity is decided before the first outcome or its exposed port is
  decoded, so it outranks a malformed exposure. A third result is never
  demanded.

Replay is one-shot because it completes inside one builtin poll: under an
exclusive lazy claim, the owner replaces the builtin checkpoint with a WHNF
checkpoint in the same value-access transition, so a later route cannot
restart it. If replay ever gains a yield or callback boundary, this argument
fails; give replay resumable state or a replay-progress test.

Why `ListEffect`: it already supplies ordered choice, failure, cut, and
fixpoint as a managed, resumable list checkpoint. Construction adds only
branch-local state, so it is a state transformer over that search
(`BuilderState -> List [value, BuilderState]`), not a second search engine.
The rejected design ran construction in an isolated reflection-task
interpreter with a Rust-side journal. Its search state was root-heavy and
outside the managed graph. Moving it beneath a traced checkpoint would store
roots in the graph. Leaving it outside lost progress on route loss: a later
route restarted the search and repeated completed operations. It also gave
construction far more interpreter capability than pure construction needs. Do
not route construction through a reflection or task machine again
([decision](../Decisions.md#net-construction-is-pure-state-over-ordered-listeffect-search-plus-hidden-strict-netlist-replay)).

## Polarity

Every port has a sign: `+` provides a value and `−` consumes one. Each wire
joins a `+` port to a `−` port, and a net's exposed port is `+` by fiat, like
`Data`. `NetBuilder::try_finish` enforces it on templates
(`interaction_net/polarity.rs`), and runtime nets carry it as a lightweight
polarity type; see "Enforcement" and "Runtime Polarity Type" below.

| Node | Ports | Signs |
| --- | --- | --- |
| `Data` | `[data]` | `[+]` |
| `Operator` | `[input, result]` | `[−, +]` |
| `Bind`, application | `[ap, arg, result]` | `[−, −, +]` |
| `Bind`, function | `[fn, result, arg]` | `[+, −, +]` |
| `Fan`, copy | `[input, left, right]` | `[−, +, +]` |
| `Fan`, merge | `[input, left, right]` | `[+, −, −]` |
| `Erase` | `[input]` | either |

- **Bind.** Its auxiliaries are always `[−, +]`. Only the principal's sign
  separates a function from an application. The crossed `Bind >< Bind` join
  is what keeps every reconnected wire `+`-to-`−`.
- **Fan.** The principal's sign is opposite its auxiliaries'. A copy
  duplicates a value. A merge superposes consumers, and user-built merges are
  allowed.
- **Erase.** Its sign is free. In `−` position it discards a value. In `+`
  position it is an error value, the analog of `void`.
- **Rewrites.** Every rewrite of a polarized active pair yields a polarized
  net (polarity-type preservation). Debug builds check it on every rewrite.
- **Construction.** A constructed net whose component is unreachable from the
  exposed port is miswired. Reduction may still leave disconnected garbage.
- **Enforcement.** `NetBuilder::try_finish` checks signs and connectivity in
  every build. It checks the builder's own topology before splicing out
  tunnels, so a violation names constructed ports, and a `.copy 1` tunnel
  passes a sign through. A violation is `NetBuildError::Polarity`, with the
  closing wire or exposed port and the earlier rules that forced it. The
  happy path allocates only the union-find; the explanation is rebuilt on
  failure.
  - A Rust-built template that fails panics through `finish`, because it is
    a constructor bug.
  - A user netlist reports an ordinary evaluation failure that names the
    user's constructors and ports, such as "port 3 of `.bind` #1" or
    "output 1 of `.copy 2` #1".
  - A test fixture that is deliberately unpolarized or disconnected opts out
    with `unpolarized_for_test` or `disconnected_for_test`, and states why.

## Runtime Polarity Type

- **Remote polarity.** Each stored link is a typed reference: the peer port
  plus the peer's sign, packed into the link word's reserved bit at no extra
  space. An unwired port has no sign.
- **Moving references.** Rewrites mostly move references:
  - fusing two sockets writes the two references verbatim;
  - a new port that stands in for a detached socket takes that socket's sign;
  - a port that binds to an existing reference takes the opposite sign.

  Internal wires between new nodes take the sign of the old port each copy
  replaces. Node rules never assign signs from a table.
- **Debug checks.** Release builds only move bits. Debug builds check two
  things:
  - every wire joins opposite signs;
  - each created node satisfies its rule as an expected peer type:
    - a `Bind`'s first auxiliary references a provider and its second a
      consumer;
    - `Data` references a consumer;
    - an `Operator`'s input references a provider and its result a consumer;
    - a `Fan`'s branch references agree and oppose its principal's;
    - an interface anchor references a provider.
- **Templates.** Template wires are stored provider-first, from
  `try_finish`'s sign solution. A sign component the constraints leave free,
  such as an isolated `Bind >< Bind` pair, is oriented arbitrarily, because
  preservation holds for any valid typing.
- **Logical copies.** Materialization binds the copied node where its cursor
  stood. The node's auxiliary signs follow its rule, and each new cursor
  stands for the source port across that link.
- **Invariants.** Test builds run `RuntimeNet::check_invariants` after every
  gateway transition on nets of up to 4,096 nodes. It checks four things:
  - links are symmetric, with opposite signs;
  - the active map equals the principal-to-principal wires;
  - copy frontiers and cursors name each other;
  - obligated cursors are unpaired.

  The randomized tests in `runtime/tests/random_nets.rs` reduce generated
  polarized nets in random orders and require one normal form.
- **Test-only links.** A hand-built test net wired with the test-only untyped
  `connect`, or one instantiated from an `unpolarized_for_test` template,
  skips the checks.

## Runtime Identity and Graph State

- Generic `SharedRuntimeNet<S>` ownership remains inside the interaction-net
  implementation. Core values instead carry the private `CoreRuntimeNet`
  facade: exactly one non-rooting managed edge (`ManagedCoreNetEdge`, latched
  at 8 bytes on x86-64). It holds no weak observer, domain route, or cached
  provenance. Inspection, mutation, duplication, identity comparison, and
  root projection each require a matching `RuntimeValueAccess`. Public
  construction boundaries establish runtime provenance; registered roots and
  collector debug validation recheck it. `EvaluationRuntimeId` remains
  globally unique and identifies the value domain.
- Core-net templates are instantiated under a matching `RuntimeValueAccess`
  (`construct_managed_core_net`) or from an already domain-qualified core
  net. Returned frontier observations and contention records preserve the
  facade rather than exposing their generic shared owner. Prepared
  logical-copy sources likewise retain exact-domain provenance and reject
  installation into another domain.
- Ordinary core-net inspection and mutation is available only through a
  private scoped view derived from the matching runtime value-access region.
  Durable net handles retain identity and provenance, but cannot inspect an
  interface, step topology, prepare a copy, or extract a result on their own.
  Evaluator code closes the view before invoking Glam callables or operators.
  Normalization leases are hidden inside a same-net scoped callback and close
  before it returns. The bracketed contention wait remains a narrow,
  documented transitional exception until its dedicated integration
  checkpoint is completed.
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

Versioned frontier observations invalidate nonterminal pair state when the
source topology changes. `Stuck` is the terminal exception: if the exact pair
is now authoritatively stuck, dispatch propagates that structured failure even
from a stale observation instead of rescanning unrelated topology.
The durable observation stores only its source, endpoint, and topology
revision. Source inspection retains the root anchor just long enough to
validate the cursor handoff; a disturbance makes the evaluator reconstruct the
path from its retained normalization request. Disturbance epochs belong to
contention records, not frontier observations.

## Reduction and External Work

Only principal-principal pairs reduce. Ordinary topology rules rewrite under
the runtime mutex. Work delegated to a specialization follows this sequence:

1. claim the exact pair under the mutex, inside the driver's normalization
   batch;
2. release the mutex when the batch closes;
3. copy out the immutable request data: a call rechecks its exact claim
   and copies in a separate read, while a cursor copies while claiming;
4. run callable, operator, or cursor work; and
5. reacquire only the owning runtime long enough to complete, block, or mark
   that same pair stuck.

A claim therefore spans several critical sections; every later one rechecks
it, and a stale holder fails quietly.

Do not rediscover work by scanning active-pair collections, remove elements
from the middle of queues, or hold source and target runtime mutexes together.
An `Erase >< RemoteCursor` pair has no shortcut: it demands normal cursor
materialization, after which the ordinary erasure rule applies.

An active pair may be linked to itself: one pair node's auxiliary port can be
wired to another auxiliary port of the same pair, as in an applied identity.
Every rewrite therefore detaches the pair's complete auxiliary boundary,
reading each neighbor before clearing any link, and resolves its replacements
along paths through such links. Closed loops vanish, and an eraser on a loop
creates nothing. Rules allocate replacement nodes in a fixed order, which the
payload edge-transition predictions rely on.

Core specialization performs each inspection or mutation through its
same-runtime scoped net view. Durable frontier and dependency records identify
work across scheduler boundaries; they do not carry that view or a managed
borrow. Source and target copy steps therefore open separate checked regions,
never one region spanning both nets.

Core `Bind >< Data` semantic work is owned by a private, thread-bound callable
claim guard. Acquisition succeeds only while the reduction's exact pair
remains claimed. The guard is consumed by one exhaustive disposition: copied
net, operator, installed callable checkpoint (see Core Specialization),
permanent structured failure, or release. It cannot enter a worklist or poll
result. Release and unwind make the pair ready again. A callable that must
wait does so as a blocked callable checkpoint, never as a blocked call. Stale
acquisition is a quiet non-acquisition, not a terminal claim outcome.

Profiling counts outcomes at the mutation boundary. With
`interaction-net-profiling`, reduction and callable-checkpoint counters are
recorded in `CoreRuntimeNetAccess` only after the authoritative runtime
mutation reports its result. An evaluator's intent is never counted: an
attempt whose exact pair or generation lost a race records a stale admission
or nothing. Inline callable-WHNF transitions record the exact shared-budget
delta. Ordinary builds compile every counter out: no observer lookup, branch,
or atomic update.

## Logical Copies and Cursors

A logical copy is target-owned `CopyState` containing:

- the shared source runtime;
- a reverse `frontiers` map from stable source ports to live target cursor
  nodes; and
- source-to-target fan-site translation.

There is deliberately no source-node to target-node history. Embedded payloads
are duplicated through the access-qualified payload duplicator. Copied target
nodes may reduce or disappear immediately, so their former source identity is
not useful provenance.

Creating a logical copy is also lock-separated. The source's immutable exposed
port is captured in a prepared copy-source token before target mutation begins;
installing `CopyState` and its first cursor then holds only the target runtime
mutex. This applies even when source and target are the same shared runtime.

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

The hierarchy here is a work-ownership property, not a claim that every
recursive program terminates. A promise or fixpoint may make a closed net
contain itself as data. Sharing may then revisit the same source runtime where
fresh template instantiation would produce an unbounded chain of equivalent
calls. Such partial evaluation may diverge or block on its semantic value
dependency, but cursor materialization itself never introduces a back edge of
mutually held work claims: a target cursor points into its closed source, and
the source never acquires a cursor into that target merely because it was
copied.

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

`NetSpecialization` (`interaction_net/model.rs`) declares associated types
only. It has no methods, and its payloads need no `Clone`:

- `Data` and the unary `Operator`, duplicated only through an
  access-qualified `RuntimeNetPayloadDuplicator`;
- `RuntimeSource`, the identity one net keeps for another;
- `WaitToken`, compared to reject a stale wakeup;
- `StuckReason`, kept on a pair that policy cannot reduce; and
- `CallableCheckpoint`, a linear `Send + 'static` payload that generic code
  moves and visits but never clones, compares, or formats.

Core binds them to `Value`, `CoreOperator`, `CoreRuntimeNet`,
`CoreWaitToken`, `EvaluationHalt`, and `Box<NetWhnfState>` (the impl is in
`eval/net.rs`). Callable interpretation (`eval/net.rs`) and operator
execution (`eval/operator.rs`) are evaluator code and run outside the runtime
mutex.

- **`Bind >< Data`.** A callable already in WHNF is classified at once:
  - `Value::Net` installs a logical-copy cursor at the net's exposed port;
  - a builtin or partial builtin becomes `CoreOperator::Builtin`;
  - a function or dictionary becomes `CoreOperator::Applicable`;
  - any other value fails, and the pair stays stuck with that
    `EvaluationHalt`.

  Operator completion fuses the inevitable unary-function bind join: it
  removes the application `Bind` and callable `Data`, then connects the
  operator principal to the former argument neighbor and its auxiliary to the
  former result neighbor.
- **Callable checkpoint.** A lazy or promised callable is first driven to WHNF
  inline, spending the net machine's borrowed step budget; a retry within
  one outer poll never gets a fresh budget. A callable that resolves within
  budget leaves no checkpoint. Only budget exhaustion or a real dependency
  replaces the `Data` node in place with a runtime-only `CallableCheckpoint`
  node holding the boxed WHNF state and a generation.
  - It has one port, never appears in a template, and is never copied: cursor
    materialization blocks on it. Its only rule is with `Bind`. Any other
    partner, `Fan` and `Erase` included, is stuck, so a checkpoint is never
    duplicated or erased.
  - `NetWhnfState` and `RegionalWhnfWork` wrap the same canonical
    `WhnfState`. Installing, claiming, and publishing move it between them
    without walking, duplicating, or rooting its values.
  - The pair becomes ready again or blocks on the exact wait. A later claim
    takes the state, drives it, and then publishes the next generation,
    finishes into a copy or operator as above, or fails the pair. At a
    dependency, the claim publishes the complete state and ends, the
    dependency is admitted outside managed access, and the pair blocks only if
    the same pair and generation are still current. A failed or killed
    dependency also fails the pair. A stale generation is rejected quietly and
    never publishes a competing result.
  - The payload needs `Send`, not `Sync`: the runtime mutex and the exact
    claim exclude shared access. A claimed state is bound to its non-`Send`
    value-access region and is republished or restored before that region
    closes.
  - The payload's managed edges are traced through the net's payload walk. It
    is never a root, and a claimed state exists only under mutator admission,
    so there is no untraced gap.
  - **Why boxed.** Boxing keeps `RuntimeNode<CoreSpecialization>` at its
    ordinary size (96 bytes on x86-64; unboxed, 128). Checkpoints exist only
    after suspension, so one allocation per spill beats enlarging every node.
    The test `regional_whnf_and_callable_checkpoint_layout_baseline` in
    `eval/whnf.rs` holds the sizes. `Gc<WhnfState>` was rejected: `Trace`
    requires `Send + Sync`, a claim cannot move state out of a managed
    allocation, and every successor checkpoint would cost a managed
    allocation. Revisit only if the state fits without enlarging the node or
    profiling shows the indirection matters.
- **`Operator >< Data`.** It runs synchronously and yields `Data` or another
  `Operator`; a returned operator is bind-wrapped for the next argument.
  Operators only construct: saturated application, builtin, access, and
  computation operators yield lazy `Data` instead of evaluating inside the
  claim, so a core operator never waits, and no blocked operator state
  exists. An error leaves the pair stuck with that `EvaluationHalt`.
- Core uses explicit `CoreOperator` enum values rather than opaque Rust
  closures. There is no error operator: a permanent failure is the stuck
  pair's `StuckReason`.

Ordinary `Value::Function` application is evaluator-owned semantic staging and
does not expose the function's staged net to callable classification. When
`Data(Value::Function)` meets `Bind`, the fused splice above installs
`CoreOperator::Applicable`. Applying it yields one lazy application of the
function to that argument (`LazyValue::from_application_in`). If arguments
remain, that lazy's WHNF is a `Value::Function`, so another application
requires another explicit `Bind`.

Raw `Value::Net` is opaque closed data already in WHNF; ordinary application
rejects it as non-callable. When a `Data(Value::Net)` node instead meets a
`Bind` inside an interaction net, callable classification installs a
logical-copy cursor at the raw net's exposed interface. This runtime call
reduction is the only implicit operation that opens the net. Opening
`FunctionValue::stage()` through this raw-net path would break value-level
partial application and is forbidden.

Builtins, copying, and erasure otherwise treat `Value::Net` like closed data;
they do not project its exposed agent. A net-backed `Value::Lazy` represents
the explicit zero-arity bridge and must produce `Data` when observed.
`FunctionValue` staging is the positive-arity bridge: partial application only
attaches arguments and never inspects the intermediate interface. Saturation
must produce `Data`; an early `Data` is left to ordinary interaction rules and
may become stuck as later arguments are attached. The source form for both
bridges is `net_arity N Net` through `import 'std`.
The same module provides the ordinary `interaction_net` construction builtin;
source programs may use either explicit `>>=`/`=>>` or the built-in front
end's `do` notation, which lowers away before net construction runs.

Every authoritative shared-runtime mutation advances a topology revision.
Outside a normalization batch it also advances the disturbance epoch and
wakes followers; inside a batch, disturbance and notification are deferred
until the batch closes. Core evaluator batches are closure-scoped to one net
and one matching managed-access region. Crossing to another net closes the
current batch first, as does returning a callable or operator action to the
semantic evaluator. Reading the immutable payload of an already claimed call
is quiet, while completing, blocking, or failing that claim is an authoritative
mutation. If one observer encounters an active pair already claimed by another
evaluator, it waits for that exact runtime to be disturbed and retries; a
claimed pair must never be misreported as quiescence. Cursor dependencies
similarly treat a source pair disappearing between inspection and claim as
progress and refresh their frontier.

## Deliberate Limits

- Node IDs and fan sites are not recycled.
- The scheduler is correctness-oriented. Configured background workers can
  evaluate sparks and thereby reduce shared nets, but runtime work stealing and
  finer-grained wake indexes are not implemented.
- Direct fan histories remain potentially large.
- Stuck pairs are retained for inspection but reflection does not yet expose
  them.
- Dictionary applicability remains supported. Revisit that path when the
  persistent lazy dictionary design changes the representation.
