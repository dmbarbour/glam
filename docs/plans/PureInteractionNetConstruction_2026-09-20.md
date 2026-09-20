# Pure Interaction-Net Construction Plan — 2026-09-20

Status: planned. This is the focused W6G.1f.3h transition from the generic
reflection-task interpreter used by `interaction_net` to ordinary pure
evaluation composed with the existing `ListEffect` search primitives. The
parent plan is
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

`interaction_net` currently creates `LazySource::NetConstruction`. Its
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

The plan does not migrate `IsolatedEffectSearch` into the GC graph. It removes
that dependency from net construction.

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
Construction internals such as the next port and operation journal are in a
separate protected component and cannot be addressed through user paths.

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
  reverse_operations: StrictList ConstructionOperation,
  user_state: DictWithHiddenAbstractGlobalPathControlEntry,
}
```

The precise field encoding is an implementation decision. It must remain an
ordinary traceable semantic value graph. It must not be an opaque payload
which secretly contains `Value`, `Gc`, or `Root` edges.

The builder operations are pure state transitions:

- `.bind` allocates three logical ports and prepends a bind operation;
- `.copy N` evaluates and validates `N`, allocates `N + 1` ports, and
  prepends a copy operation;
- `.data Value` allocates one logical port and prepends the unforced `Value`;
  and
- `.wire Left Right` evaluates and validates both port tokens and prepends a
  wire operation.

Port tokens retain the current edge-free construction brand and positive
logical ID. A token from another invocation fails before replay. Creating a
construction brand once for the public `interaction_net` application is
permitted: the brand is identity-only host data and contains no Glam edge.

Operation insertion is O(1). The journal is a strict reverse spine so branch
prefixes share structure. Replay restores source order once, after unique
selection. No operation record may contain a deferred structural field;
`.data` payload is the deliberate exception because it is net data, not
builder syntax.

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
calling replay. The selected builder record and its reverse journal are fully
structural at that boundary.

### Hidden replay primitive

Introduce an internal `InteractionNetFromNetlist` operation (final Rust name
may follow the existing builtin convention). It is not bound by `import
'std` and cannot be named by ordinary source.

It accepts one selected, normalized construction record and:

1. validates sequential allocation and operation arities;
2. maps logical ports to `NetBuilder` ports;
3. validates wires and the exposed port;
4. constructs the managed `CoreRuntimeNet`; and
5. returns `Value::Net`.

Replay may duplicate ordinary value edges into the resulting net under the
matching value-access region. It must not force `.data` payloads. It performs
no callbacks and does not retain raw values after access closes.

The first implementation may keep replay synchronous, matching current
behavior. Record replay length as a user-sized loop for later budgeting; do
not complicate this transition with a second resumable machine unless a
fixture demonstrates that it is necessary.

## Ownership and GC Invariants

1. Search progress belongs to the existing managed `ListEffect` machinery.
2. Builder state and operation records are ordinary traceable values.
3. No runtime root is stored inside builder state, operation records, the
   selected outcome, or any managed lazy checkpoint.
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

### PNC1 — Strict semantic netlist and hidden replay

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

### PNC2 — State-over-`ListEffect` foundation

- Build the pure `Builder A = BuilderState -> ListEffect Outcome` composition
  using the existing list-effect primitives.
- Implement `.r`, `.seq`, `.alt`, `.fail`, and `.cut` by delegation to that
  machinery rather than introducing another search engine.
- Add branch-local state and hierarchical `.get/.set` without exposing the
  protected construction fields.
- Reserve the pure control key with `abstract_global_path`, store its value
  inside `user_state`, and latch whole-state capture/replacement semantics
  before implementing `reset/shift`.
- Verify ordered blocking, state rollback across failed alternatives, state
  retention through selected cut, and first-two-result observation.

Stop for review if this layer requires a new producer route, a root stored in
the semantic graph, or a second implementation of ordered list search.

### PNC3 — Fixpoint and delimited-control parity

- Compose `.fix` with `ListEffectFix` while retaining the documented reset
  scoping and one-fixpoint-value-per-alternative behavior.
- Implement `.reset/.shift` as pure task-local control over the builder state
  and continuation, guided by the existing interpreter oracle and the pure
  handler model in `docs/Design.md`.
- Cover nested and missing keys, continuation invocation, hidden-state
  preservation under nonempty paths, whole-state clearing and restoration,
  cut inside reset, reset inside alternatives, cross-invocation rejection,
  and fix/reset interaction.
- Keep the pure control helper general enough for other pure handlers, but do
  not turn this checkpoint into a replacement for the effectful reflection
  task interpreter.

Exit: the documented standard task-local API has behavioral parity without a
reflection machine.

### PNC4 — Pure net-builder API

- Implement `.bind`, `.copy`, `.data`, and `.wire` as builder-state
  transitions.
- Create the brand once, allocate monotonic positive IDs, prepend strict
  operation records, and return branded port tokens.
- Ensure argument evaluation happens in ordinary evaluator work before the
  state transition commits.
- Cover port-count overflow, non-integer/negative copy counts, wrong operation
  arities, wrong token kinds, and foreign invocation tokens.

Exit: running a construction program yields only ordinary list-effect
outcomes containing strict semantic netlists.

### PNC5 — Unique selection and public composition

- Implement the first-two-outcomes uniqueness check in ordinary evaluation.
- Validate and extract the exposed branded port from the selected outcome.
- Change public `interaction_net` application to construct the pure runner,
  selector, and hidden replay pipeline.
- Preserve laziness and memoization: constructing `interaction_net Effect`
  does not itself run `Effect`, and demanding the resulting lazy runs and
  replays the selected construction at most once.
- Decide at this checkpoint whether the reusable pure helper is best kept as
  a cached semantic object or a small closed family of internal composition
  builtins. Either choice must remain traceable, evaluator-owned, independent
  of `g_syntax`, and suitable for eventual expression in `.g`.

Exit: production construction no longer enters `IsolatedEffectSearch`.

### PNC6 — Legacy route removal

- Remove `NetConstructionMachine`, `NetConstructionPoll`,
  `NetConstructionState`, and `InteractionNetEffects`.
- Remove `LazyTaskWork::NetConstruction` and
  `LazySource::NetConstruction` once no compatibility caller remains.
- Remove the root-bearing Rust construction journal and its isolated host.
- Reconcile compile-exhaustive lazy-source, producer-route, registered-root,
  persistent-edge, and autonomous-obligation inventories.
- Update `docs/agent_context/interaction_nets.md`, `docs/Syntax.md`, and source
  architecture notes to describe the implemented pure boundary.

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
./scripts/test_interaction_net_profiling.sh
```

Exit: W6G.1f.3h is complete and W6G.1f.3i can close the remaining
state-bearing lazy-producer inventory.

## Deferred Performance Work

- Budget or incrementally replay very large selected netlists.
- Batch construction calls or arguments to reduce intermediate semantic
  values.
- Refine strict operation records alongside `ValueRepresentationRefinement`.
- Move reusable pure handler definitions into Glam source once bootstrap and
  cache boundaries make that preferable.
- Add profiling counters for result-prefix demand, builder operations, and
  replay separately.

These are not reasons to preserve the generic reflection machine in the
construction path.
