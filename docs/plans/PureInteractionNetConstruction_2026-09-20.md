# Pure Interaction-Net Construction Plan — 2026-09-20

Status: active; PNC0-PNC4 completed on 2026-09-20, and the post-PNC4 focused
remediations and PNC5 completed on 2026-09-21. PNC6 legacy removal is next. The
[post-PNC4 review](../reviews/PureInteractionNetConstructionPNC4_2026-09-20.md)
found no demonstrated result defect. Its private diagnostic contract,
no-replay evidence, replay-order/API/malformed-record latches, and future-phase
partitioning are now closed. This is the focused
W6G.1f.3h transition from the generic reflection-task interpreter used by
`interaction_net` to ordinary pure evaluation composed with the existing
`ListEffect` search primitives. The parent plan is
[`ResumableWhnfEvaluation_2026-09-12.md`](ResumableWhnfEvaluation_2026-09-12.md).

## Purpose

Make interaction-net construction an ordinary pure Glam computation instead
of a specialized reflection machine.

The public operation remains conceptually:

```text
interaction_net ConstructionProgram
```

Internally it becomes:

```text
interaction_net ConstructionProgram
  = interaction_net_from_netlist
      (select_unique
        (run_builder_with_list_effect ConstructionProgram empty_builder_state))
```

The existing `ListEffect` primitives remain responsible for ordered choice,
failure, cut, and list fixpoints. A pure builder layer threads branch-local
construction state through that search. Only after ordinary evaluation has
selected one complete construction does a hidden primitive validate and
replay the strict netlist into a runtime interaction net.

This split gives the two layers distinct jobs:

- ordinary evaluation handles functions, application, laziness, suspension,
  state, delimited control, and ordered search;
- the hidden replay primitive decodes one already selected netlist, validates
  its construction-local port tokens and topology, and builds the net.

The hidden primitive is not another evaluator. It performs no effect
dispatch, search, callbacks, reflection, or dependency waits.

## Why `ListEffect` Is the Right Foundation

Glam already has a pure, resumable implementation of the relevant search
semantics:

- `ListEffectReturn` yields one result;
- `ListEffectSeq` lazily concatenates continuation results in source order;
- `ListEffectAlt` represents ordered alternatives;
- an empty list represents failure;
- `ListEffectCut` selects the first result; and
- `ListEffectFix` implements the supported alternative/fixpoint interaction.

The front-end already uses `ListEffect` for pure conditional and match
selection. Its production implementation is an inspectable managed
checkpoint, not a reflection task, and already preserves exact WHNF/list
progress across yield and dependency suspension.

Interaction-net construction should compose with that machinery instead of
reimplementing `.alt`, `.cut`, and `.fix` in `IsolatedEffectSearch`.

Choice alone is not the full builder. Construction also needs branch-local
state, and its documented API includes all standard task-local effects. The
appropriate model is therefore a state transformer over `ListEffect`, not a
mutable journal beside it.

## Current Implementation and Defect Boundary

Before PNC5, `interaction_net` created `LazySource::NetConstruction`. Its
`NetConstructionMachine` owns an `IsolatedEffectSearch<InteractionNetEffects>`
and a persistent Rust `ConstructionJournal`. The generic task interpreter
provides standard task-local effects; specialized requests implement
`.bind`, `.copy`, `.data`, and `.wire`. Once the search produces exactly one
result, Rust forces the exposed port and replays the journal through
`NetBuilder`.

This is semantically serviceable, but it is the wrong ownership boundary for
the W6G transition:

- the generic effect search is root-heavy external task state;
- moving it beneath a traced lazy checkpoint would place roots inside the
  managed value graph;
- retaining it outside the graph recreates the route-loss and replay problem
  W6G is eliminating; and
- construction receives a much larger reflection interpreter than its pure
  semantics require.

The plan does not migrate `IsolatedEffectSearch` into the GC graph. PNC5C has
removed that dependency from production net construction; the legacy source
and machine remain only until PNC6 deletes them.

## Selected Semantics

### State transformer over ordered search

Use the following logical type:

```text
Builder A = BuilderState -> ListEffect { value:A, state:BuilderState }
```

The standard operations have these meanings:

```text
r value       = \state -> list.r { value, state }

seq op next   = \state ->
  list.seq (op state)
           (\outcome -> next outcome.value outcome.state)

alt left right = \state ->
  list.alt (left state) (right state)

fail          = \_state -> list.fail

cut op        = \state -> list.cut (op state)
```

This is explanatory pseudocode, not a requirement to add a Rust
`Builder<A>` type. The representation should be ordinary semantic functions,
lists, dictionaries, and strict builder records wherever practical.

Each alternative receives the same persistent input state. An operation
returns a new state rather than mutating shared state. A failed branch cannot
contaminate a later branch, and shared journal prefixes remain shared through
ordinary persistent values.

Ordered search is authoritative. If the left alternative blocks, the whole
search blocks; readiness must not select the right alternative. `cut` selects
the first successful result in this same order.

### Task-local state and control

The combined builder API continues to expose the documented standard
task-local operations:

- `.r`, `.seq`, `.alt`, `.fail`, `.cut`, and `.fix`;
- `.get` and `.set` over a hierarchical dictionary; and
- `.reset` and `.shift` with task-local delimited continuations.

`.get` and `.set` operate on a `user_state` component of `BuilderState`.
Construction internals such as the next port, constructor journal, and wire
journal are in a separate protected component and cannot be addressed through
user paths.

Delimited-control state is deliberately different: it lives *inside*
`user_state`, beneath an implementation-owned `abstract_global_path` key. The
handler has authority for that key; ordinary source cannot forge it, name it,
or directly inspect the stored reset frames. Nevertheless, the hidden entry
is part of the dictionary returned by `.get []`. Saving that whole value and
later passing it to `.set []` therefore checkpoints and restores both visible
user state and the current reset stack.

This is intentional, not a representation accident. It lets advanced pure
programs checkpoint a complete local computation state for coroutine-like
control or effectful backtracking without gaining the ability to interpret or
edit the hidden continuation data. The ordinary state operations consequently
have these rules:

- `.get []` returns the complete state dictionary, including the inaccessible
  control entry;
- `.set [] State` replaces the complete state, so a newly written dictionary
  without that entry also clears the active reset stack;
- nonempty user paths cannot address the hidden entry and otherwise preserve
  it; and
- restoring a checkpoint does not relax task/handler-invocation identity:
  captured continuations remain invalid outside their owning invocation.

Do not split control into a sibling `BuilderState::control` field. Doing so
would make `.get []`/`.set []` incomplete and lose this deliberate composition
between state and delimited control.

The pure-handler model in `docs/Design.md` is the semantic reference for
state and delimited control. The existing task interpreter remains the
behavioral oracle during migration. In particular:

- alternatives start from the same state;
- a successful `cut` continues with the selected branch's state;
- reset frames are task-local and keyed;
- `shift` fails when its key is outside reset scope;
- fixpoint hides the reset scope as documented; and
- continuation use must retain the existing state-restoration behavior.

Do not approximate `reset/shift` merely because ordinary construction samples
rarely use them. First latch the current behavior with focused tests, then
implement the pure composition. If exact parity requires a reusable pure
control helper, build that helper in the evaluator rather than retaining the
reflection machine only for net construction.

### Construction operations

`BuilderState` has at least these logical fields:

```text
{
  brand: ConstructionBrand,
  next_port: PositiveInteger,
  reverse_constructors: StrictList ConstructorDescriptor,
  reverse_wires: StrictList WirePair,
  user_state: DictWithHiddenAbstractGlobalPathControlEntry,
  sequence_stack: StrictList SequenceFrame,
}
```

The precise field encoding is an implementation decision. It must remain an
ordinary traceable semantic value graph. It must not be an opaque payload
which secretly contains `Value`, `Gc`, or `Root` edges.

The compact constructor program is:

```text
ConstructorDescriptor =
    BindTag
  | [CopyTag, output_count]
  | [DataTag, payload]

WirePair = [left_port_id, right_port_id]
```

The tags are implementation-owned atoms. Wire endpoints are protected positive
integer IDs, not branded public port values. The builder operations are pure
state transitions:

- `.bind` allocates three logical ports and prepends `BindTag`;
- `.copy N` evaluates and validates `N`, allocates `N + 1` ports, and
  prepends `[CopyTag, N]`;
- `.data Value` allocates one logical port and prepends
  `[DataTag, Value]` without forcing `Value`; and
- `.wire Left Right` evaluates and validates both port tokens and prepends a
  protected pair of their logical IDs to `reverse_wires`.

Port tokens retain the current edge-free construction brand and positive
logical ID. A token from another invocation fails before replay. Creating a
construction brand once for the public `interaction_net` application is
permitted: the brand is identity-only host data and contains no Glam edge.

Each insertion adds one O(1) journal entry after any unavoidable construction
of the operation's result tokens. Both journals are strict reverse spines, so
branch prefixes share structure. Constructor order remains authoritative
because it assigns logical ports, node order, and copy sites. Explicit wires
are commutative with constructor allocation once their public operands have
been demanded and brand-checked, so replay creates all constructors first and
then applies wire pairs in their original relative order.

`next_port` remains an O(1) allocation cursor even though replay can derive it.
Replay checks the derived constructor-port total against it. Constructor
descriptors do not repeat their derived logical ports: bind contributes three,
copy contributes `N + 1`, and data contributes one. No descriptor may contain
a deferred structural field; `.data` payload is the deliberate exception
because it is net data, not builder syntax.

A fully columnar encoding with separate tag, copy-count, and data-value stacks
would retain marginally less list-record overhead. It is deliberately deferred:
the compact descriptors remove the unbounded copy-port duplication without
introducing several synchronized streams. Because the protocol is private, it
may be columnarized later if profiling justifies the additional invariants.

### Unique selection

The runner observes no more than the first two completed outcomes:

- no result: fail with the established “no successful result” error;
- one result: use its exposed port and state; and
- two results: fail with the established ambiguity diagnostic and recommend
  `.cut`.

The second-outcome check remains lazy. It does not normalize an unbounded
result list. If determining whether a second outcome exists blocks, unique
selection blocks rather than prematurely accepting the first.

The selected exposed value is evaluated and decoded as a branded port before
calling replay. The selected builder record and both reverse journals are
fully structural at that boundary.

### Hidden replay primitive

Introduce an internal `InteractionNetFromNetlist` operation (final Rust name
may follow the existing builtin convention). It is not bound by `import
'std` and cannot be named by ordinary source.

It accepts one selected, normalized construction record and:

1. replays constructor descriptors in source order while assigning logical
   ports implicitly;
2. validates descriptor shapes, copy counts, and the derived `next_port`;
3. maps logical IDs to `NetBuilder` ports;
4. validates and applies wire pairs in their original relative order;
5. validates the exposed branded port and completed topology;
6. constructs the managed `CoreRuntimeNet`; and
7. returns `Value::Net`.

Replay may duplicate ordinary value edges into the resulting net under the
matching value-access region. It must not force `.data` payloads. It performs
no callbacks and does not retain raw values after access closes.

The first implementation may keep replay synchronous, matching current
behavior. Record replay length as a user-sized loop for later budgeting; do
not complicate this transition with a second resumable machine unless a
fixture demonstrates that it is necessary.

## Ownership and GC Invariants

1. Search progress belongs to the existing managed `ListEffect` machinery.
2. Builder state, constructor descriptors, and wire pairs are ordinary
   traceable values.
3. No runtime root is stored inside builder state, constructor descriptors,
   wire pairs, the selected outcome, or any managed lazy checkpoint.
4. Construction port tokens are edge-free and branded per invocation.
5. `.data` stores the original semantic value as a normal managed edge and
   does not demand it.
6. The hidden replay primitive owns no durable search or continuation state.
7. Losing one evaluator route cannot restart completed list-effect,
   state/control, builder, selection, or replay work.
8. A lazy `.data` payload may point back to the construction or resulting net
   without turning a host root into the owner of that cycle.
9. Reflection heap, logging, environment, shared heap, task, and host I/O
   capabilities remain unavailable. Only the documented pure task-local API
   and construction operations are exposed.

## Transition Plan

### PNC0 — Contract and behavioral baseline

Status: complete on 2026-09-20.

- Inventory the exact API currently visible through `ConstructionHost` and
  divide it into documented task-local behavior, construction operations, and
  incidental generic-interpreter capability.
- Add focused parity fixtures for every documented standard operation,
  including `get/set`, `reset/shift`, and `fix` interactions which existing
  net-construction tests do not cover.
- Latch current zero-result, ambiguous-result, foreign-port, invalid-topology,
  and structured-error diagnostics before moving the implementation.
- Add deterministic route-loss fixtures around construction search and replay;
  do not accept repeated passing runs as evidence for a concurrency ordering.

Exit: the behavior being preserved is executable and incidental reflection
capabilities are explicitly excluded from the target.

Completion record: `ConstructionHost` injects exactly this API:

| Class | Members |
| --- | --- |
| Standard task-local | `r`, `seq`, `alt`, `fail`, `cut`, `fix`, `get`, `set`, `reset`, `shift` |
| Interaction-net construction | `bind`, `copy`, `data`, `wire` |
| Not injected | `heap`, `exit`, `task`, `log`, `env` |

The shared-heap flag and exit-capability flag are both false. Ordinary
language annotations, including `anno refl:`, remain ordinary evaluator
behavior rather than members of the construction API: they may delay or
enrich evaluation according to the general annotation contract, but they do
not give the construction handler a reflection, heap, logging, environment,
or task capability. A successful annotation still exposes its unchanged
target; annotation failure and divergence retain their ordinary behavior.

An executable API-shape test now prevents accidental capability growth.
Source fixtures cover branch-local state rollback, nonempty-path preservation
of the hidden reset stack, whole-state reset-stack capture/clear/restore,
`shift`, and fixpoint hiding then restoring reset scope. The pre-existing
construction fixtures remain the baseline for return/sequence,
alternative/failure/cut, zero and ambiguous outcomes, foreign port brands,
structured error context, lazy `.data`, invalid exposure/topology, copy
validation, and memoization.

An instrumented deterministic fixture counts completed construction
operations and replay. An uninterrupted construction performs one `.data`
transition and one replay. The named ignored regression
`construction_search_survives_route_loss_without_replaying_completed_operations`
forces route loss after that `.data` transition and currently observes the
known legacy defect: the later route restarts isolated search and raises the
operation count from one to two. PNC5 must make this test pass and remove its
ignore marker; the baseline deliberately does not assert that replay is valid
behavior.

### PNC1 — Strict semantic netlist and hidden replay

Status: complete on 2026-09-20.

- Define the ordinary semantic encoding for builder state, branded ports, and
  reverse operation records.
- Extract the current `NetBuilder` replay and validation into the hidden
  `InteractionNetFromNetlist` primitive.
- Test the primitive directly with valid bind/copy/data/wire combinations,
  malformed records, nonsequential ports, wrong brands, invalid topology, and
  lazy data payloads.
- Prove by source inventory that the primitive cannot effect-dispatch, wait,
  call user code, or retain roots.

Exit: a strict selected record can build the same runtime net without the
construction search machine.

Completion record: PNC1 implemented the following provisional, port-explicit
semantic schema. It uses strict value lists so an empty `user_state` remains an
actual field rather than disappearing under Glam's undefined-dictionary-entry
rule:

```text
selected = [builder_state, exposed_port]
builder_state = [brand, next_port, reverse_operations, user_state]

bind = [BindTag, port, port, port]
copy = [CopyTag, input_port, output_port...]
data = [DataTag, port, payload]
wire = [WireTag, left_port, right_port]
```

This is a historical implementation record, not the PNC4 target. PNC4A
replaces the single port-explicit journal with the six-field compact state:

```text
builder_state = [brand, next_port, reverse_constructors, reverse_wires,
                 user_state, sequence_stack]

constructor = BindTag | [CopyTag, output_count] | [DataTag, payload]
wire        = [left_port_id, right_port_id]
```

Constructor replay derives every allocation port. Wire pairs retain only
positive logical IDs after their public tokens have been demanded and
brand-checked. The PNC1 wire tag and the repeated bind/copy/data port tokens
therefore disappear. `next_port` remains both the construction cursor and a
replay consistency check.

In the PNC1 schema, the tags are implementation-owned abstract global paths
and `brand` and `port` are edge-free opaque identity tokens; every other field
is an ordinary traceable semantic value. Structural lists must contain no byte
or deferred segments. The `.data` payload is copied as an edge without
observation and is the only field permitted to remain lazy. PNC4A preserves
these boundary properties while replacing journal-local port tokens with
protected numeric IDs.

`Builtin::InteractionNetFromNetlist` is implemented by one callback-free
regional transition and is deliberately absent from `import 'std`. The
legacy construction machine now adapts its selected journal to this exact
schema before replay, so both current construction and the later pure runner
share validation and `NetBuilder` lowering. This adapter is transitional:
PNC4A migrates the primitive and adapter together to the compact schema, PNC4
then constructs that semantic state directly, and PNC5 removes the old search
machine.

Direct PNC1 fixtures cover a net containing bind/copy/data/wire, malformed
outer and operation records, zero-port copies, nonsequential allocation,
foreign brands, incomplete topology, and an undemanded lazy data payload.
PNC4A replaces the port-explicit cases with compact-schema checks for malformed
constructor descriptors, inconsistent `next_port`, malformed or out-of-range
wire IDs, and unconsumed structural fields. A separate source inventory rejects
reflection/effect imports, scheduler or WHNF boundaries, evaluator callbacks,
waits, and runtime-root construction in the replay module.

### PNC2 — State-over-`ListEffect` foundation

#### PNC2A — Direct-result reuse of ordered list search

Completed on 2026-09-20.

- Extend the existing managed list-effect recipe with direct-result flat-map
  and first-result forms. These variants consume ordinary result lists rather
  than wrapping each continuation in another effect dictionary, but reuse the
  same `RegionalListFront`, suspension, and source-order machinery.
- Verify left-to-right blocking, lazy tail retention, direct flat-map order,
  and first-two-result observation without adding another producer route or
  list-search reducer.

#### PNC2B — Pure state-transformer composition

Completed on 2026-09-20.

- Represent one outcome as the strict record `[value, builder_state]` and a
  builder operation as an ordinary callable from state to a list of those
  records.
- Add a small closed family of hidden evaluator builtins for `.r`, `.seq`,
  `.alt`, `.fail`, and `.cut`. Their saturated applications only construct
  ordinary application/list recipes; ordered search delegates to PNC2A.
- Verify branch-local state, rollback across failed alternatives, state
  retention through selected cut, and source-ordered blocking.

This chooses the "small closed family of internal composition builtins"
representation previously left until PNC5. The family is evaluator-owned,
traceable, independent of `g_syntax`, and absent from `import 'std`.

#### PNC2C — Protected task-local state

Completed on 2026-09-20.

- Add hierarchical `.get/.set` over only the `user_state` field, using the
  existing regional key-list, WHNF, and dictionary-update machinery.
- Reserve the pure control key with `abstract_global_path` and initialize its
  value inside `user_state` before implementing `reset/shift`.
- Latch that `.get []` captures and `.set []` replaces the complete
  `user_state`, including hidden control state, while nonempty paths preserve
  the hidden entry and cannot address protected construction fields.
- Verify lazy paths/intermediates, missing and invalid paths, whole-state
  replacement, and preservation of the control entry by ordinary nested
  updates.

The implementation keeps only one new regional adapter around existing
components: path conversion uses `RegionalKeyList`, reads use `AccessMachine`,
and nested writes use `DictUpdate`. The adapter owns the protected four-field
builder record and rebuilds it only after the delegated operation completes.
No runtime root, reflection task, or additional search state enters the
semantic graph.

Stop for review if this layer requires a new producer route, a root stored in
the semantic graph, or a second implementation of ordered list search.

### PNC3 — Fixpoint and delimited-control parity

Status: complete on 2026-09-20.

The reference monolith in `docs/Design.md` and the current reflection handler
agree on three details which the PNC2 flat-map representation does not yet
make explicit:

- `.seq` contributes a continuation which may be captured by `.shift`;
- `.cut` is a delimiter inside that continuation and must select before the
  continuation outside the cut runs; and
- `.fix` hides the complete reset/control scope while its function runs, then
  restores that scope independently for each result alternative.

PNC3 keeps the external pure-builder shape
`BuilderState -> List [value, BuilderState]`. It defunctionalizes the active
continuation into ordinary semantic values; it does not introduce another
evaluator or search engine. The provisional strict encoding separates the
protected active sequence from the reset state which `.get []` and `.set []`
must capture and replace:

```text
builder_state = [brand, next_port, reverse_operations, user_state,
                 sequence_stack]

sequence_stack = StrictList SequenceFrame     # head is the next frame
reset_stack    = user_state[CONTROL_KEY]       # outer to inner

sequence = [SequenceTag, continuation]
cut      = [CutTag]

reset  = [ResetTag, key, outer_sequence]
resume = [ResumeTag, caller_sequence]
```

The tags are implementation-owned abstract global paths. Continuations and
saved stacks are normal traced value edges. Missing `CONTROL_KEY` means an
empty reset stack, so `.set [] {}` clears active reset scope without erasing
the monadic continuation currently executing `.set`. The fifth builder-state
field is protected from `.get/.set` and always present, using the empty strict
list when there is no active sequence. Builder state is therefore one
fixed-arity private record rather than a sum of four- and five-field shapes.
Both stacks are strict while continuation values remain lazy. The
construction brand already present in `BuilderState` is the invocation
identity and therefore need not be duplicated in every frame.

This five-field shape records the PNC3 implementation. PNC4A splits its
`reverse_operations` field into `reverse_constructors` and `reverse_wires`,
making the protected record six fields wide. State/control transitions remain
representation-neutral: they transport both journals unchanged just as they
currently transport the one provisional journal.

This separation mirrors the oracle rather than weakening the checkpoint rule:
a reset frame stored in `user_state` carries the protected sequence snapshot
to resume when its body returns. Consequently `.get []` still captures the
complete reset continuation, while `.set []` can clear or restore reset scope
without cancelling the operation which performs that update.

#### PNC3A — Reference matrix and structural contract

- Translate the existing reflection-oracle fixtures into a table of pure
  builder transitions before changing composition: normal and missing shift,
  nested keys, cut inside reset, reset inside alternatives, complete-state
  clear/restore, cross-invocation continuation use, and fix/reset hiding.
- Add strict sequence/reset frame encode/decode helpers and require the
  fixed five-field builder-state record, including an explicit empty sequence
  stack. Reject malformed hidden control records at the evaluator boundary.
  Do not demand continuation fields while decoding the structural stacks.
- Keep the control representation private to the evaluator. It is ordinary
  traceable data under the hidden key, not a Rust opaque payload or root.

Exit: every later transition has one named oracle case and one concrete
semantic record shape.

#### PNC3B — Defunctionalized sequence, return, and cut

- Change builder `.seq` from direct result flat-map to pushing a protected
  sequence frame and running its operation. Builder `.r` becomes the common return
  dispatcher: it pops a sequence frame and applies its continuation, pops a
  reset frame and restores its saved sequence on normal return, restores a
  caller sequence at a resume frame, or emits the terminal `[value, state]`
  outcome when both stacks are empty.
- Route successful `.get`, `.set`, and later construction operations through
  that same dispatcher; no valid builder operation may bypass active control.
- Implement `.cut` with a strict cut frame. Return stops at that frame and
  exposes one candidate to `ListEffect` `FirstResult`; only the selected
  candidate is redispatched into the continuation outside the cut.
- When a captured continuation contains cut frames, its resume adapter
  reinstalls the corresponding first-result stages in order. This prevents a
  continuation which escapes a reset from silently losing its cut delimiters.
- Preserve `.alt` as two applications over the same persistent input state,
  including the same immutable control stack.

Exit: ordinary sequence and cut use one traceable continuation stack, retain
PNC2 branch-local state behavior, and still delegate ordered selection to the
canonical list-effect reducer.

#### PNC3C — Pure reset, shift, and continuation invocation

- Convert reset/shift keys with `RegionalKeyConversion`, preserving exact
  lazy/promise suspension and source ownership.
- `.reset Key Operation` moves the current protected sequence into a reset
  frame under `CONTROL_KEY`, clears the active sequence, and runs `Operation`.
- `.shift Key Function` scans from the top for the nearest matching reset,
  removes that reset and all inner frames from the active state, and passes a
  captured continuation to `Function`. A missing key fails with the existing
  “not in reset scope” diagnostic.
- A captured continuation contains the construction brand, the active
  sequence, and the immutable reset frames inside the target. Invoking it
  appends a resume reset frame containing the caller's current sequence,
  reinstalls the captured inner reset frames and sequence, then returns its
  argument into that continuation. Reaching the resume frame restores the
  caller sequence and continues there; the caller's outer reset frames remain
  in place.
- Compare decoded construction-brand identity on invocation and reject a
  continuation used with another builder invocation. Reuse within the owning
  invocation remains non-affine.

Exit: reset/shift and escaped continuation calls match the reflection oracle
without a task object, host callback, or root in the semantic graph.

#### PNC3D — One fixpoint future per ordered alternative

- First add a direct `ListEffectFix` regression in which the second
  alternative observes a different future than the first. The present
  implementation publishes only the first head and returns its existing tail;
  that is insufficient for the documented `fixListFn` semantics.
- Extend the existing managed list-effect recipe with an alternative index.
  Index `N` creates one managed promise, evaluates the fix function with that
  promise, skips exactly `N` ordered results with `RegionalListFront`, assigns
  the selected result to the promise, and emits it with a lazy `N + 1` recipe
  as the tail. Exhaustion publishes the established empty-list assignment and
  terminates the result list.
- Preserve source-order blocking, one promise per observed alternative,
  bounded resumption, and route-loss memoization. Do not add another list
  walker or producer route.

Exit: generic `ListEffectFix` implements the design's one-future-per-choice
contract and has a forced-order regression.

#### PNC3E — Builder fixpoint composition

- `.fix Function` saves the active control stack, clears it in the function's
  builder state, and adapts `Function` to PNC3D `ListEffectFix`.
- The generic fix promise carries the complete selected builder outcome. The
  value passed to the user's `Function` is a lazy projection of field zero;
  recursively demanding it retains the ordinary promise-cycle diagnostic.
- Each fixed outcome restores the saved outer control stack into its updated
  user state and enters the common return dispatcher. Reset frames are thus
  unavailable inside the fix body but restored after every alternative.
- Keep this adapter as ordinary hidden builtins and list recipes. Do not move
  reflection fix frames, task-owned promises, or branch journals into the
  builder.

Exit: builder fixpoints preserve branch-local state, receive one value future
per alternative, and hide then restore reset scope.

#### PNC3F — Parity, ownership, and closure

- Cover nested and missing reset keys, normal and escaped continuation
  invocation, cut inside reset, a captured cut delimiter, reset inside
  alternatives, hidden-state preservation under nonempty paths, whole-state
  clearing and restoration, cross-invocation rejection, recursive
  self-observation, and fix/reset interaction.
- Force lazy key, continuation, and fix-function suspension at explicit poll
  boundaries; add route-loss/collection coverage for the new managed builtin
  and list-effect states.
- Reconcile the raw-value, durable-owner, recursive-constructor, persistent
  edge, WHNF, and builtin exhaustiveness inventories.
- Keep the helper private and no broader than the pure task-local API. The
  effectful reflection interpreter retains its independent transactional,
  task, heap, diagnostic, and host-I/O responsibilities.

Exit: the documented standard task-local API has behavioral parity without a
reflection machine.

Completion record: the pure builder now uses two deliberately distinct
control stores. A protected fifth builder-state field holds the active
sequence/cut stack, while the implementation-owned `CONTROL_KEY` entry in
`user_state` holds reset/resume frames. This is the smallest representation
which preserves both reference rules: `.get []` and `.set []` can capture,
clear, and restore reset scope, but the `.set` operation cannot erase the
sequence which is currently executing it. Empty protected sequence state is
represented by an explicit empty strict list, keeping the private builder
record fixed-width and leaving optionality inside the field it describes.

The executable reference matrix is:

| Reference behavior | Pure representation | Direct latch |
| --- | --- | --- |
| normal/nested/missing shift | nearest matching `Reset` frame in `CONTROL_KEY` | `hidden_builder_reset_shift_handles_nested_keys_cut_and_missing_scope` |
| lazy prompt conversion | `RegionalKeyConversion` before frame inspection | `hidden_builder_reset_shift_resumes_lazy_keys_and_captured_cut` |
| cut captured by shift | strict `Cut` frame plus reconstructed `FirstResult` stage | `hidden_builder_reset_shift_resumes_lazy_keys_and_captured_cut` |
| reusable same-invocation continuation | branded partial `Resume` builtin | `hidden_builder_captured_continuation_is_reusable_only_with_its_invocation` |
| foreign invocation rejection | decoded construction-brand identity | `hidden_builder_captured_continuation_is_reusable_only_with_its_invocation` |
| reset-scope branch isolation | immutable input state per ordered alternative | `hidden_builder_reset_scope_is_branch_local_across_alternatives` |
| whole-state clear | replace `user_state`; preserve protected sequence | `hidden_builder_whole_state_clear_does_not_erase_the_active_sequence` |
| whole-state restore | reset frames are ordinary traced data below `CONTROL_KEY` | `hidden_builder_whole_state_checkpoint_restores_reset_scope` |
| malformed hidden state | strict frame decoders at the evaluator boundary | `hidden_builder_rejects_malformed_control_records` |
| one future per alternative | indexed managed `ListEffectFix` recipe | `list_effect_fix_allocates_one_future_for_each_observed_alternative` |
| fix hides/restores reset scope | clear before body; per-outcome restore adapter | `hidden_builder_fix_uses_independent_alternatives_and_restores_control` |
| recursive fix observation | lazy value projection from the complete promised outcome | `hidden_builder_fix_reports_recursive_future_observation` |
| exact path/state suspension and route loss | retained builtin checkpoint plus exact promise dependencies | `builder_checkpoint_survives_path_and_state_dependencies_without_replay` |
| lazy key suspension and route loss | counted key source beneath the retained builtin checkpoint | `builder_checkpoint_observes_a_lazy_reset_key_once_across_route_loss` |
| later fix alternative route loss | retained selection route across fix and restoration adapters | `later_builder_fix_alternative_survives_route_loss_without_replay` |

`.r`, successful state operations, reset return, resume return, and fixed
outcome restoration all converge on one return dispatcher. `.cut` stops that
dispatcher at a strict delimiter, delegates selection to the canonical
list-effect first-result recipe, and only then resumes the outer sequence.
No new producer route, task, reflection callback, host root, or independent
list walker was introduced.

Generic list-effect fixpoints now carry an alternative index. Observing
alternative `N` creates one managed promise, reevaluates the function with
that promise, skips exactly `N` ordered outcomes through `RegionalListFront`,
publishes the selected outcome, and exposes a lazy `N + 1` recipe. This is
intentionally simple and may revisit earlier alternatives; it preserves the
one-future-per-choice contract and exact suspension behavior without adding a
second search mechanism.

The raw-value, durable-owner, persistent-edge, regional-constructor, and WHNF
censuses classify the new helpers as access-bounded evaluator work. Focused
control/fix fixtures, all source inventories, Clippy, the complete Rust suite,
and the interaction-net profiling matrix passed for the PNC3 implementation.
The post-PNC3 review's additional forced-order and malformed-key fixtures now
close the narrower gaps in the original completion record. PNC4 may therefore
begin without changing PNC3's architecture or semantics.

### PNC4 — Pure net-builder API

Status: complete on 2026-09-20. The implementation used the following
checkpoints rather than combining state representation, operand demand, and
API assembly in one change.

#### PNC4A — Compact replay schema and initial state

##### PNC4A.1 — Compact state and replay migration

Status: complete on 2026-09-20.

- Replace PNC1's port-explicit operation list with
  `reverse_constructors` and `reverse_wires`. Reuse the PNC1 bind, copy, and
  data tags, but remove the wire tag and every derived allocation-port field.
- Atomically migrate the shared protected-state codec to
  `[brand, next_port, reverse_constructors, reverse_wires, user_state,
  sequence_stack]`. Update PNC2/PNC3 encode/decode, tracing, malformed-state,
  terminal-replay, and fixed-arity fixtures in the same checkpoint; do not
  temporarily accept both shapes.
- Decode `BindTag`, `[CopyTag, output_count]`, and `[DataTag, payload]` as the
  only constructor descriptors. Replay them in source order while assigning
  consecutive logical IDs and building the logical-ID-to-`NetBuilder` mapping.
- Validate that the derived allocation cursor equals `next_port`. Then replay
  `[left_port_id, right_port_id]` pairs in their original relative order,
  range-checking both endpoints before calling `NetBuilder::try_wire`.
- Update the legacy construction adapter to emit the compact schema. Preserve
  `.data` payloads as unforced semantic edges and preserve one callback-free
  value-access region around replay.
- Replace PNC1's derivable-port fixtures with malformed-descriptor,
  inconsistent-`next_port`, malformed/out-of-range-wire, exposed-brand, and
  topology fixtures. Keep the lazy-data and replay-boundary source proofs.

Completion record: builder state is now the single six-field compact record.
Constructors and wires occupy separate reverse journals; constructors derive
logical port IDs during source-order replay, and wires retain only ID pairs.
The legacy construction adapter emits the same representation. Focused replay,
malformed-state, bounded-mutator, durable-owner, registered-root, and
persistent-edge checks pass without a compatibility decoder for the former
five-field shape.

##### PNC4A.2 — Initial pure-builder state

Status: complete on 2026-09-20.

- Add one initial-state encoder which receives a construction brand and emits
  port ID one, two empty reverse journals, initialized user state, and an
  explicit empty sequence stack. PNC5, not an individual operation, will
  allocate the brand once per public `interaction_net` application.
- Keep protected fields raw while ordinary state/control operations merely
  transport both journals. Construction transitions may validate the fields
  they edit, but must not traverse either complete reverse journal on append.

Completion record: `initial_builder_state` receives the runner-owned brand and
constructs port ID one, empty constructor and wire journals, the initialized
hidden user-state dictionary, and an explicit empty sequence stack. Its
structural fixture verifies the brand identity and all six fields; production
brand allocation remains deliberately deferred to PNC5.

#### PNC4B — Bind and data transitions

Status: complete on 2026-09-20.

- Implement `.bind` and `.data` beneath the existing managed builder builtin
  checkpoint. Demand and decode the builder state, allocate monotonic positive
  IDs with checked arithmetic, prepend `BindTag` or `[DataTag, payload]`, and
  enter the common return dispatcher. Neither descriptor repeats its derived
  port IDs.
- Return branded port tokens as ordinary strict lists. `.data` retains its
  payload as an unforced semantic edge; only the state is demanded before the
  transition is published.

Completion record: bind and data now share the managed builder checkpoint's
resumable state demand, checked monotonic allocation, compact constructor
journal prepend, and common return dispatcher. Bind returns three branded
ports and data one; a failing lazy data payload remains wholly undemanded.

#### PNC4C — Copy and wire operand demand

Status: complete on 2026-09-20.

- Evaluate `.copy` count and `.wire` ports through resumable regional WHNF
  work before publishing a replacement builder state. Wire evaluation remains
  left-to-right, matching the legacy request interpreter.
- Require a nonnegative integer copy count, check `count + 1`, target capacity,
  and logical-port exhaustion, and reject non-port or foreign-brand wire
  operands before journal insertion.
- Prepend `[CopyTag, count]` without retaining its returned port vector.
  Convert successfully decoded wire tokens to logical IDs and prepend only the
  strict pair; the hidden replay boundary remains responsible for range and
  completed-topology validation.
- Force yield, exact dependency, failure, route-loss, and collection at each
  operand boundary; equal terminal values are not sufficient evidence that an
  operand or transition was not replayed.

Completion record: construction operands use one ordered regional-WHNF queue
inside the same traced builder checkpoint. Copy validates its count before
state demand and retains only the count descriptor. Wire evaluates left,
right, then state, validates both branded tokens, and retains only their IDs.
A forced-route-loss fixture publishes all three exact dependencies in order,
collects between handoffs, and verifies one committed wire. Compact replay,
invalid-count, foreign-token, exhaustion, and lazy-payload fixtures cover the
remaining transition boundary.

#### PNC4D — Private API assembly and closure

Status: complete on 2026-09-20.

- Assemble one private API dictionary containing the PNC2/PNC3 standard
  task-local operations plus bind, copy, data, and wire. The builtin arities
  are the operation-arity contract; direct malformed internal calls may fail
  defensively, but source-visible partial application is not an arity error.
- Cover fixed-state preservation, independent branch-local rollback of both
  journals, port-count overflow, non-integer/negative copy counts, wrong token
  kinds, foreign invocation tokens, and lazy data payloads.
- Reconcile builtin, raw-value, durable-owner, persistent-edge, recursive-cell,
  and WHNF inventories before closing the phase.

Completion record: the private dictionary exposes exactly `r`, `seq`, `alt`,
`fail`, `cut`, `fix`, `get`, `set`, `reset`, `shift`, `bind`, `copy`, `data`,
and `wire`; no reflection, host, heap, logging, environment, exit, or task
capability enters the builder. Failed alternatives roll back constructor and
wire journals together, while builder fixpoints preserve the state supplied
by their caller. Builtin compilation and the raw-value, durable-owner,
persistent-edge, recursive-identity, bounded-access, registered-root, and WHNF
censuses are reconciled with the new regional operand queue.

Exit: every private construction operation maps one builder state to ordinary
list-effect outcomes carrying the fixed protected state and compact journals.
Applying a complete construction program to the private API and initial state
remains PNC5 work.

### PNC5 — Unique selection and public composition

#### PNC5A.0 — Recursive effect-to-builder interpretation

Status: complete on 2026-09-21. The captured `shift` continuation is an
ordinary callable returning an effect, not an effect itself.

- Add the state-threading analogue of the canonical list-effect `Run` rule:
  `run_builder_effect Effect State = Effect.eff PrivateApi State`. This is not
  a second effect protocol. The `eff` header still distinguishes effects from
  ordinary functions. A continuation may use `eff:(\api value -> ...)` as a
  source-level convenience, but applying that function still produces an
  effect for the runner to interpret; `EffectApply` is not a separate builder
  protocol.
- Treat the effect operands of `seq`, `alt`, `cut`, `reset`, `shift`, and
  `fix` exactly as list-effect operands are treated: interpret them recursively
  against the same private API before supplying builder state. Keep the lower
  return dispatcher and protected-state transitions as ordinary internal
  state-transformer operations.
- Present a captured shift continuation as an ordinary callable
  `Value -> Effect`. Applying it produces an `eff` value for the same runner
  to interpret, rather than exposing the evaluator's raw builder-state
  callable or treating the continuation itself as an effect.
- Add effect-wrapped nested-operation fixtures before public cutover. Retain
  the existing direct builder fixtures as lower-layer checks only where they
  do not cross an effect-operand boundary.

Exit: nested construction effects obey the same recursive `eff` discipline as
`ListEffect`; no source-level branch or continuation is accidentally treated
as an already-lowered builder operation.

#### PNC5A — Private runner assembly

Status: complete on 2026-09-21. The runner retains effect-header demand and
application to the private builder API and initial state beneath the source
checkpoint.

- Allocate one construction brand and one fixed initial builder state for each
  runner instance. Apply the construction effect to the exact PNC4 private API,
  then apply the resulting builder operation to that state.
- Keep this composition private and non-authoritative while verifying zero,
  one, and multiple ordinary list-effect outcomes, exact operation arities,
  and unchanged propagation of failures from effect application and builder
  execution.
- Keep the runner evaluator-owned, independent of `g_syntax`, and suitable for
  eventual expression in `.g`; do not introduce a cached compiler-owned
  semantic object merely to package its outcome stream.

#### PNC5B — Retained first-two selector

Status: complete on 2026-09-21. The selector retains the first outcome and
list-front progress, and does not observe a third result after ambiguity.

- Implement uniqueness by observing at most the first two outcomes: none is a
  failed construction, one is selected, and two proves ambiguity without
  traversing the remainder.
- Give the selector an explicit retained regional state containing its current
  list-front work and first outcome. Do not hide it in a large edit to
  `RegionalNetMachine` or restart list observation after yield, dependency,
  route loss, or collection.
- Force empty, unique, multiple, yielded, exact-dependency, failed, and
  route-loss schedules. Count list-front production so equal terminal failures
  cannot conceal replay.

#### PNC5C — Public lazy composition and cutover

Status: complete on 2026-09-21. Public construction now runs through the
pure builder, selector, exposed-port demand, and compact replay. The legacy
source is temporarily allowed as dead code pending PNC6 removal.

- Validate the selected outcome, demand and decode its exposed branded port,
  and pass the strict selected record to hidden replay.
- Change public `interaction_net` application to construct this runner,
  selector, exposed-port, and replay pipeline lazily. Constructing
  `interaction_net Effect` must not run `Effect`; demanding the result runs the
  selected construction and replay at most once.
- Add `eval:{op:'net_construction}` exactly once at this public boundary for
  failures from effect application, builder execution, selection, exposed-port
  demand, and replay. Keep private operand demands transparent: the legacy
  `copy_count` frame is not a compatibility requirement, and the builder does
  not introduce parallel wire-port, reset/shift-key, path, state, or exposed-
  port frames.
- Cut production over only after the public result and failure baselines agree
  with this selected policy.

#### PNC5D — Route-loss and diagnostic closure

Status: complete on 2026-09-21. The obsolete ignored legacy route-loss test was
retired after adding pure-runner route-loss and collection fixtures.

- Replace the ignored legacy route-loss regression with a pure-runner fixture
  and force loss plus collection at effect application, builder execution,
  first- and second-outcome observation, exposed-port demand, and replay.
- Count construction-program and continuation demand, first and second
  selector chunks, and exposed-port demand through externally visible lazy
  callbacks. Replay itself is a synchronous terminal transition with no yield
  or callback boundary; assert memoized net identity after completion rather
  than introducing a production probe solely to count replay calls. Assert the public
  `net_construction` frame is present exactly once at every failure boundary
  and that no private operand-role frame leaks through it.
- Re-run the ordinary, aggressive-collection, profiling, and source-inventory
  verification before closing the production cutover.

Exit: production construction no longer enters `IsolatedEffectSearch`.

Completion record: the source-level construction fixtures, focused public
failure/context matrix, budget-one route-loss/collection fixtures, first-two
tail non-observation test, aggressive-GC construction fixtures, profiling
script, source inventories, and full ordinary test suite pass. The old
`LazySource::NetConstruction` and `NetConstructionMachine` remain unreachable
from public construction until PNC6 removes them.

### PNC6 — Legacy route removal

#### PNC6A — Preserve the pure construction identity protocol

- Move `ConstructionBrand`, `ConstructionPortId`, the edge-free opaque token
  family, and their codecs out of legacy `construction.rs` into a small pure
  construction-identity module.
- Migrate the PNC4 builder, compact replay, tests, and the still-live legacy
  adapter to that module first. Compile and run the focused brand/token tests
  before deleting any producer route.

#### PNC6B — Remove the legacy producer route

- Remove `NetConstructionMachine`, `NetConstructionPoll`,
  `NetConstructionState`, and `InteractionNetEffects`.
- Remove `LazyTaskWork::NetConstruction` and
  `LazySource::NetConstruction` once no compatibility caller remains, followed
  by the root-bearing Rust construction journal and its isolated host.
- Remove the legacy `copy_count` evaluator frame; the public
  `net_construction` frame remains authoritative.

#### PNC6C — Inventory and documentation closure

- Reconcile compile-exhaustive lazy-source, producer-route, registered-root,
  persistent-edge, and autonomous-obligation inventories.
- Update the automatic-context inventory in `docs/Syntax.md` plus
  `docs/agent_context/interaction_nets.md` and source architecture notes to
  describe the implemented pure boundary.
- Search for the legacy machine, journal, isolated host, and `copy_count`
  spelling, then run the full routine and profiling checks.

Exit: net construction has no dedicated state-bearing lazy-producer route.

### PNC7 — Verification and closure

Run the full matrix below under ordinary and aggressive-collection fixtures.
Where scheduling matters, use barriers or explicit poll boundaries to force
both relevant orderings.

- basic and mixed bind/copy/data/wire construction;
- zero, one, and multiple successful outcomes;
- cut selecting the first success;
- a blocked left alternative not selecting a ready right alternative;
- failed-branch journal and state rollback;
- `get/set`, `fix`, and `reset/shift` parity;
- foreign brands and malformed tokens;
- inconsistent `next_port` summaries and malformed or out-of-range wire IDs;
- lazy `.data` payloads, including a backedge to the owning construction;
- invalid exposure, unwired ports, duplicate wires, and other builder errors;
- yield and exact dependency suspension in list search, state/control,
  builder argument evaluation, unique selection, and exposed-port demand;
- route loss and collection at each suspension boundary;
- one-shot construction execution and one-shot hidden replay;
- collection of construction/checkpoint cycles after external roots drop; and
- absence of reflection, task, heap, environment, logger, and host I/O caps.

Count completed branch prefixes, builder transitions, unique selections, and
replay calls so terminal-value equality cannot hide duplicated work.

Then run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
./scripts/check-interaction-net-profiling.sh
```

Exit: W6G.1f.3h is complete and W6G.1f.3i can close the remaining
state-bearing lazy-producer inventory.

## Deferred Performance Work

- Budget or incrementally replay very large selected netlists.
- Batch construction calls or arguments to reduce intermediate semantic
  values.
- Profile compact constructor descriptors before considering a columnar tag,
  copy-count, and data-value representation alongside
  `ValueRepresentationRefinement`.
- Move reusable pure handler definitions into Glam source once bootstrap and
  cache boundaries make that preferable.
- Add profiling counters for result-prefix demand, builder operations, and
  replay separately.

These are not reasons to preserve the generic reflection machine in the
construction path.
