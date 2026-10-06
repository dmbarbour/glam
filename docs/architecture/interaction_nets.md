# Interaction-Net Runtime Architecture

How the bootstrap evaluates interaction nets at runtime: the cursor-WHNF net
driver (`eval/net.rs`), the mutable runtime net (`interaction_net/runtime.rs`
and `runtime/{graph,rewrite,cursor}.rs`), and the managed core-net cell
(`core_net.rs`). Construction and the regression-sensitive invariants live in
[`../agent_context/interaction_nets.md`](../agent_context/interaction_nets.md).
The evaluator's side (lazies, WHNF, budgets, coordinator) is in
[`evaluation.md`](evaluation.md), and managed values are in
[`values.md`](values.md). Decisions are cited by slug from
[`../Decisions.md`](../Decisions.md).

## Ownership

| Piece | Source | Role |
| --- | --- | --- |
| `RuntimeNet<S>`, `RuntimeNetCell<S>` | `interaction_net/runtime.rs` | graph state; its mutex, revisions and batches |
| rules and wiring | `runtime/rewrite.rs`, `runtime/graph.rs` | boundary rewrites, debug polarity checks |
| cursors | `runtime/cursor.rs` | copies, cursor claims, source frontiers |
| `SharedRuntimeNet<S>` | `runtime.rs` | `Arc` owner for the generic test specialization only |
| `ManagedCoreNetCell` | `core/managed/recursive_cells.rs` | managed allocation embedding one core `RuntimeNetCell` |
| `CoreRuntimeNet`, `CoreRuntimeNetAccess` | `core_net.rs` | one-edge facade and its scoped view |
| `NetWhnfMachine`, `NetDriver`, claim guards | `eval/net.rs` | interface demand and semantic handoffs |

The generic runtime contains no core policy. Evaluator code reaches a core net
only through `CoreRuntimeNetAccess`, which borrows a matching
`RuntimeValueAccess` and cannot outlive that access region.

## Runtime Net State

- **`RuntimeNet`.**
  - `nodes` maps each never-reused `NodeId` to a node plus three inline
    link words.
  - `active` is a `BTreeMap` holding one `ActivePairState` per
    principal-to-principal wire, keyed by the lower node ID.
  - `copies` and `cursor_obligations` hold the logical copies this net owns
    and the demand on unpaired cursors.
  - `exposed` is the port of an `Interface` anchor added at instantiation.
- **Instantiation.** `RuntimeNet::new` is eager: it duplicates every template
  payload and writes every wire provider-first.
- **`RuntimeNetCell`.** It adds one `Mutex` over the net and its batch state,
  plus an atomic topology revision. Its `RuntimeNetDisturbance` is an
  edge-free `Arc` with an epoch, a condvar and a closed flag, so waiting
  never retains the net. Dropping the cell wakes every waiter. Publishers
  take the disturbance mutex under the net mutex; waiters never take the net
  mutex.

## Active Pairs and Claims

| State | Meaning | Left by |
| --- | --- | --- |
| `Ready` | reducible | a claim; a published checkpoint's block or failure |
| `Claimed` | one evaluator owns the transition | rewrite, block, stuck, or release |
| `BlockedCallableCheckpoint { generation, wait }` | callable WHNF waits on `wait` | exact retry → `Ready`; dependency failure → `Stuck` |
| `BlockedCursor { cursor, blockage }` | cursor waits on a dependency, or is `Stable` | dependency resolution, or a join by its peer cursor |
| `Stuck(reason)` | `NoRule` or a specialization `EvaluationHalt` | never; kept for diagnostics |

- **Claim.** `reduce_pair_with_gateway` writes `Ready → Claimed` in place
  under the net mutex. A pure rule finishes in the same critical section and
  deletes the entry. Cursor and semantic work finishes in a later one, after
  rechecking its exact claim: a `Call`, an `OperatorCall`, a
  `CallableCheckpointCall` with its generation, or the cursor's copy and
  remote. A stale holder fails quietly.
- **Release and unwind.** `CoreCallClaim`, `CoreOperatorClaim`,
  `CoreCheckpointClaim` and `CursorClaimGuard` restore on drop. A held claim
  becomes `Ready` again, and a taken checkpoint state goes back into its
  node. A poisoned net is skipped.

## Rewrite Rules

| Active pair | `ReductionKind` | Result |
| --- | --- | --- |
| `Bind >< Bind` | `BindJoin` | join auxiliaries crossed |
| `Fan >< Fan`, equal identity | `FanJoin` | join auxiliaries positionally |
| `Fan >< Fan`, otherwise | `FanCommute` | four fans, histories extended |
| `Fan >< Data` | `FanData` | two `Data` duplicates |
| `Fan >< Bind` | `FanBind` | two `Bind`s, two residual fans |
| `Fan >< Operator` | `FanOperator` | two operators, one residual fan |
| `Erase ><` an ordinary agent | `Erase` | erase each auxiliary |
| `RemoteCursor ><` anything | `RemoteCursor` | cursor claim, checked first |
| `Bind >< Data` | `Call` | semantic handoff |
| `Bind >< CallableCheckpoint` | `CallableCheckpoint` | semantic handoff |
| `Operator >< Data` | `OperatorCall` | semantic handoff |
| anything else, including a checkpoint with a non-`Bind` partner | `Stuck` | `Stuck(NoRule)` |

- `detach_boundary` reads every auxiliary neighbor before clearing any link,
  so a pair linked to itself becomes sockets. The rule removes the pair and
  allocates replacements in a fixed order. `attach_boundary` joins paths
  through the sockets: port to port is wired, port to eraser gets a fresh
  `Erase`, and eraser pairs and closed loops vanish.
- A new port takes the sign of the socket it replaces. `wire` keeps `active`
  exact: a new principal pair starts `Ready`, or inherits a pairless cursor
  obligation's state.

## Mutation Gateway

Every mutation runs inside `RuntimeNetMutationGateway::transition_edges`
while the net mutex is held.

- **Generic.** `DirectRuntimeNetMutationGateway` runs the update unchanged.
- **Core.** `ManagedCoreNetAccess` wraps the update in
  `with_managed_edge_state_transition`. It passes an exact leaving and
  adding `RuntimeNetEdgeTransition`: at most two node slots, one copy source,
  one pair payload and one obligation. The `*_edge_transition` predictors
  read the pre-write net and rely on the fixed allocation order. The
  stop-the-world collector visits neither side
  (decision `owner-qualified-edge-gateways`).
- **Tests.** Both gateways run `check_invariants_after_transition`.
- **Publication.** The `with_conditional_*` cell entries publish only on
  `RuntimeNetMutation::Changed`, and `with_cleanup_*` skips a poisoned net.
  `publish_mutation` advances the topology revision. Outside any batch it
  also bumps the disturbance epoch and notifies all waiters. Inside a batch
  it marks the batch dirty, and the batch publishes once at close if it is
  dirty or contended.
- **Tracing.** `try_visit_logical_payloads` `try_lock`s the net, panics on
  contention, and reads through poison.

## The Net Driver

### Entry

- **Machines.** `LazySource::NetComputation` (the zero-arity bridge) and
  `LazySource::FunctionCall` install a `NetWhnfMachine` in a managed
  net-WHNF lazy checkpoint. A function call first instantiates a small
  application-spine net around the function's stage (`attach_net_many_in`).
- **Polling.** `LazyTaskMachine::poll_net_whnf_checkpoint` (`eval/value.rs`)
  polls the machine under the checkpoint mutex, inside one access region.
  See [`evaluation.md`](evaluation.md) "Lazy Producers".
- **State.** A `NetDriver` holds the `NormalizationRequest` (root net and
  interface port) and a LIFO worklist. Items hold traced edges and IDs, never
  claims, so any item can be rebuilt from the request root.

### Work Items

| Item | Step | Next |
| --- | --- | --- |
| `RequestRoot` | `poll_interface_demand` | terminal: end the poll; else re-push, then `Cursor` or `ActivePair` |
| `Cursor` | `step_cursor` | a dependency pushes `ResumeCursorDependency`, then the dependency's item |
| `ObservedCursor` | `step_cursor` at the observed source revision | as `Cursor` |
| `ActivePair` | `step_active_pair` | handoff; a cursor-blocked pair pushes `Cursor` |
| `ObservedActivePair` | `step_active_pair` at the observed source revision | as `ActivePair` |
| `ResumeCursorDependency` | `resolve_cursor_dependency` | `Disturbed` or `Gone`: restart from the root |

`poll_interface_demand` classifies the anchor's neighbor:

- **Principal.** `Data` and `Bind` are terminal. A `RemoteCursor` gives
  `Cursor` (installing a pairless obligation) or `StableCursor`. Anything
  else gives `NormalForm`.
- **Auxiliary.** Walk the principal chain to the first active pair
  (`ActivePair`, or `StableCursor` if stably blocked), else `NormalForm`.

### Flow

```text
NetWhnfMachine::poll_in                     (one value-access region)
  loop: pop item -> open a normalization batch on the item's net
    run items while they stay on that net    (another net: push back, close)
      claim refused: no budget left      -> push back -> yield
      pure rewrite or cursor progress    -> continue
      root terminal                      -> poll outcome
      Call | Checkpoint | Operator       -> leave batch -> handoff
      Claimed pair, cursor, or batch     -> Contended
  access closes
handoff:   drive_net_semantic_action (own access regions), then re-poll
contended: wait for the net's disturbance epoch, then yield
```

- **Budget.** Every reduction costs one unit of the shared step budget:
  pure rewrites, the remote-cursor rule, and semantic claims alike. The
  runtime spends it at the claim, through an admission callback on
  `step_active_pair_with_gateway` and `step_cursor_with_gateway`; a refused
  claim reports `NotAdmitted`, leaves the net unchanged, and ends the poll.
  Observation is free: interface polls, chain walks, dependency resolution,
  and rechecking a blocked checkpoint's wait. A poll that performs no
  reduction ends in a result, handoff, contention or block, so free
  observation cannot loop.
- **Batches.** Only one evaluator may hold a net's batch; a second claimant
  gets `Contended` at once. Stepping a source's pair or cursor opens a batch
  on that source, so source-local work runs in the source.
- **Root outcomes.** `Data` is duplicated out, and the lazy replaces its net
  checkpoint with an ordinary WHNF checkpoint on that value. `Bind`,
  `NormalForm` and `StableCursor` fail as "exposed a bind" or "non-data
  normal form".
- **Handoffs.** A handoff re-queues its exact `ActivePair`. While it runs,
  `semantic_in_flight` makes other routes on the same lazy yield.

## Semantic Handoffs

`drive_net_semantic_action` runs after the batch and access region close.
Each claim opens its own access region (see [`evaluation.md`](evaluation.md)
"WHNF Submachine Flow"). The claim already paid its budget unit, so a
handoff needs no admission of its own; callable WHNF then draws on what
remains.

- **`Bind >< Data`** (`progress_exact_core_call_in`).
  1. `CoreCallClaim::fresh` checks `Claimed` and duplicates the callable in a
     quiet read.
  2. A lazy or promised callable is first driven inline by `drive_regional`
     on the shared budget (decision `inline-first-callable-spill`).
  3. The WHNF callable takes one disposition. A `Value::Net` becomes a
     logical copy and a builtin becomes `CoreOperator::Builtin`. A function
     or dictionary becomes `CoreOperator::Applicable`, fused with the bind
     join. Anything else leaves the pair `Stuck`.
  4. Budget exhaustion or a boundary instead spills. The `Data` node becomes
     a `CallableCheckpoint` at generation 0, and the pair goes back to
     `Ready`. At a boundary, `settle_callable_checkpoint_boundary` forms a
     wait after access closes. It blocks the pair only if that generation is
     still `Ready`, and fails the checkpoint if no wait can be formed.
- **`Bind >< CallableCheckpoint`.** `CoreCheckpointClaim::take` moves the
  boxed state out and drives it in place. It then finishes as above, fails,
  or publishes the next generation.
- **`Operator >< Data`.** `CoreOperatorClaim::fresh` duplicates both
  payloads, and `apply_core_operator` runs synchronously. The pair becomes
  `Data`, a `Bind` plus `Operator` awaiting the next argument, or `Stuck` on
  error. Saturated work comes back as lazy `Data`, so core operators never
  wait.
- **Blocked retries** poll their exact wait. A pending wait returns
  `EvaluationHalt::blocked(wait)`. A checkpoint whose dependency failed or
  was killed fails its pair. Otherwise the checkpoint pair becomes `Ready`
  for the next pass.

## Blocking and Failure

| Situation | Net state | Lazy route sees |
| --- | --- | --- |
| no rule | `Stuck(NoRule)` | permanent failure naming the pair |
| callable or operator failure | `Stuck(Specialization(halt))` | that `halt`, cached as the lazy's failure |
| callable dependency | `BlockedCallableCheckpoint` | `Blocked` on the exact wait, on the next pass |
| failed or killed dependency | checkpoint dropped, `Stuck` | permanent failure |
| pair, cursor or batch claimed elsewhere | unchanged | wait for disturbance, then yield |
| stale observation | unchanged | progress; re-derived from the root |
| step budget spent | nothing claimed | yield |

A stale observation of a since-stuck pair still reports `Stuck`, so the
failure propagates without a rescan.

## Logical Copies and Cursors

- **Preparing.** `prepare_copy_source` registers a temporary root for the
  source and reads its exposed port under the source mutex, before any
  target lock.
- **Starting.** `begin_copy` creates a `CopyState` with the source edge, a
  `frontiers` map and a fan-site map. It puts a principal-only
  `RemoteCursor { copy, remote }` where the callable stood, facing the bind.
- **Claiming.** A cursor claim never holds two net mutexes:

  ```text
  target lock   claim the pair or obligation; copy out cursor, copy, remote, source
  source lock   classify the frontier at `remote`; record the topology revision
  target lock   finish: materialize, join, block, or mark stable
  ```

The finish depends on the source port wired to `remote`:

| Source frontier | Target outcome |
| --- | --- |
| already a frontier of this copy | join: wire the two cursors' local neighbors; the copy retires with its last frontier; a claimed peer gives `LocalCursor` |
| principal of an ordinary node | materialize: clone it (payload duplicated, fan identity translated) and add one cursor per auxiliary |
| principal of a `RemoteCursor` | block on a `SourceCursor` observation |
| auxiliary of a node in an active pair | block on a `SourceFrontier` observation |
| auxiliary of an inactive node | walk its principal chain: a peer cursor gives `LocalCursor`, a final pair gives `SourceFrontier`, else stable |

- **Resuming.** A dependency is a `FrontierObservation` (source edge,
  topology revision, endpoint) or a local peer. Resolving it makes the
  cursor's owner `Ready`. A stable result marks the owner `Stable` instead,
  and `mark_nearest_dependency_stable` carries that up the worklist.
- **Pairless obligations.** Demand on an unpaired cursor installs an
  obligation: `Ready`, `Claimed`, `Blocked` or `Stable`. `wire` moves it into
  the pair's state when the cursor joins a pair, and node removal drops it.
- **Sharing.** Copies nest. A call net copies the function's stage, which
  copies earlier stages (captures, partial applications) down to the
  function code. Each source pair reduces once, in its source, for every
  target. Deep chains run on the worklist, not the Rust stack
  (decision `no-semantic-recursion-on-rust-stack`).

Anchors stay valid because the frontier advances only through principals.
See the agent note's "Logical Copies and Cursors" and "Fans".

## Core Specialization

`CoreSpecialization` binds `Data = Value`, `Operator = CoreOperator`,
`RuntimeSource = CoreRuntimeNet`, `WaitToken = CoreWaitToken`,
`StuckReason = EvaluationHalt` and `CallableCheckpoint = Box<NetWhnfState>`.
The impl is in `eval/net.rs`.

- **Edges.** Each core net is one `ManagedCoreNetCell`. Values, copy sources
  and driver items reach it through an 8-byte non-rooting `CoreRuntimeNet`
  edge. The lazy checkpoint traces driver state.
- **Roots.** A handoff that outlives an access region holds a registered
  `ManagedCoreNetRoot`, as `NetSemanticAction` and `CorePreparedCopySource`
  do.
- **Access.** `CoreRuntimeNetAccess` wraps the `ManagedCoreNetAccess`
  gateway and records profiling counts after commits.

See [`values.md`](values.md) "Recursive Identities" and "Tracing".

## Polarity and Invariant Checks

Link words carry their peer's sign, and release builds only move that bit
(decisions `polarized-interaction-nets`, `remote-polarity-runtime-type`).

- **Debug builds** check each new wire and each created node's rule.
- **Test builds** also run `RuntimeNet::check_invariants` after every gateway
  transition on nets of up to 4,096 nodes.
- **`runtime/tests/random_nets.rs`** reduces random polarized nets in random
  orders.

The rules are in the agent note's "Polarity" and "Runtime Polarity Type".

## Performance-Relevant Facts

**Locks and claims**

- One mutex per net guards its whole topology, and every operation takes it
  separately.
- A pure rewrite holds it for the whole step: revision check, edge-delta
  prediction, claim flip, duplication of both endpoint nodes (payloads
  included) for matching, the rewrite, active-map updates and the revision
  increment. Outside a batch, publication adds the disturbance mutex and a
  `notify_all`.
- A batch takes the net mutex once to open and once to close. It ends at
  every net switch and semantic handoff.
- A cursor step takes three locks (target, source, target). A blocked
  `step_cursor` takes a fourth.
- A semantic step also registers a root, opens an access region, takes a
  read lock to copy out payloads and a completion lock, and notifies unless
  a batch is open. A net callable adds a source lock and a second root.
- Contention blocks the polling thread on a condvar, outside managed access.

**Driver**

- Each reduction costs one budget unit, so one poll performs at most its
  budget of reductions; observation is free (see "Budget" above).
- The request root sits at the bottom of the worklist and is re-polled
  whenever the items above it finish. Each re-poll from an auxiliary walks
  the principal chain with a fresh `HashSet`. Source-frontier classification
  does the same in the source.

**Representation**

- `nodes`, `copies` and `cursor_obligations` are std `HashMap`s with the
  default SipHash hasher, so every link read or write is a hash lookup.
- `RuntimeNode<CoreSpecialization>` is 96 bytes on x86-64, mostly its
  64-byte `Value`. Each entry adds three link words.
- Every rule allocates small `Vec`s for ports, links, replacements, visited
  flags and created nodes.
- Fan commutation copies each fan's history into a new `Arc` slice. Fan
  equality compares histories, and materialization translates them
  recursively.
- Partial application, function calls and capture binding each build,
  polarity-check and eagerly instantiate a small template as a new managed
  net. Logical copies share their source instead.
- Edge-delta prediction and, in test builds, the O(net size) invariant check
  also run under the lock.
- The `glam-prof` feature (`interaction_net/profiling.rs`)
  counts committed rewrites and driver events. Ordinary builds compile the
  counters out.
