# Promise Cells — 2026-10-23

A design note, for the maintainer's review before any build, on the model
of promises that machines, reflection and requests resolve: one identity
per promise, the promise's cell held weakly by its resolver, and the store
holding no promise alive. Asked for by the maintainer (2026-10-23), after
`api-refl-try` met two identities for one promise and translated between
them by a scan. It comes before `api-refl-child-tasks`, since `.child.task`
answers with a promise.

## Today

| Piece | What it is |
|---|---|
| Cell | `ManagedPromiseCell { id, assignment, producer, completion }`, in the managed heap; `id` is a `NonZeroU64` from the runtime's monotonic counter |
| Promised value | `Value::Promised`, a strong edge to the cell |
| Store record | `Promises`: its own counter id to `Pending(root)`, then `Resolved { fulfilled }` |
| Resolver | `Value::Opaque(PromiseCapability { runtime, store id, notices })`, an edge-free external owner |
| Resolver lost | the capability's destructor queues `DeathNotice::Resolver(store id)`; after a collection the runtime breaks the promise with a commit and drops its record (`opaque-resources-reclaimed-when-unreachable`) |
| Resolution | staged in a cut's journal by store id, settled at commit under the store's lock, then published into the cell (`promise-resolution-commits-with-its-cut`) |
| Status | read from the store record; a read in a transaction observes `promise_path(store id)` in the runtime volume, which resolution writes |

What that costs:
- **Two identities.** The cell's id and the store's id name one promise.
  `.try` learns of a wait by its cell and needs the store's id to observe
  it, and `Promises::pending_with_cell` scans for it. The maintainer
  rejected the scan (2026-10-23): many promises may be in flight.
- **The store keeps every pending promise alive.** Its record roots the
  cell until the promise resolves, so a promise whose promised value no
  one holds still lives, for as long as its resolver does.

## The model

`Resolver -(strong)-> Breaker -(weak)-> Cell { id, assignment, … } <-(strong)- Promised`
(maintainer, 2026-10-23)

- **Cell.** As now: the cell's `id` is the promise's only identity. It is
  a counter, not an address, so a moving collector keeps it.
- **Promised value.** As now: a strong edge to the cell. The promised value
  alone keeps the cell alive.
- **Breaker.** A new managed family, not an opaque payload (maintainer).
  It holds the cell weakly (glam-gc's `Weak`, an ephemeron keyed on its
  value), the cell's `id`, its own status record, and the sending end of
  the runtime's notice channel.
  Its status starts pending and becomes fulfilled or broken when its
  resolution commits, so status always answers from the resolver's side,
  whether or not the cell still lives (maintainer, 2026-10-23: status per
  breaker; a `'forgotten` status, for a cell no promised value holds,
  waits for a use case, since it would need the cell's death to wake its
  readers).
- **Resolver.** A value kind, `Value::Resolver`, an edge to the breaker,
  as volume handles are edges to volume cells (`api-volume-cells`).
  Copies of a resolver share one breaker.

### A breaker or an opaque resolver

Glam sees no difference between the two: the choice is performance
(maintainer, 2026-10-23). An opaque resolver, as `PromiseCapability` is
today, would keep its status in its payload and find its cell through the
weak table by its `id`. It keeps the existing value kind, but:
- a drain of the external-owner registry checks every live owner for a
  dead lease, under the registry's lock, after every collection and at
  every host call: work in proportion to the resolvers alive, at every
  drain, the cost the scan was rejected for (`perf-external-owner-drain`);
- creating a resolver, and opening it to resolve it or read its status,
  takes the registry's lock and a lookup, and resolving takes a second
  lookup, in the weak table.

A breaker's death is found by the collector, which already visits the
dead, and resolving or reading status follows the breaker's edges. It
costs a new value kind, as volumes did, and the clarification of passive
destruction below. Kept (agent, 2026-10-23), unless the maintainer prefers
the smaller build.

### A lost resolver

When a breaker dies, its destructor queues `DeathNotice::Resolver(id)` on
the runtime's channel, as the opaque capability's does today. After the
collection, at a safe point, the runtime finds the cell through a **weak
table**, the cell's `id` to a weak reference to it, and breaks it with
"resolver lost", with a commit as today.

- The destructor sends plain data, the cell's `id`, never its weak edge,
  and touches no managed edge, so the breaker serves as its own token.
- The table is keyed by the cell's own `id`: a lookup, never a scan.
- An entry goes when its promise resolves, or when its notice arrives.
  glam-gc's `Weak` is itself a small managed cell, so an entry roots the
  breaker's weak reference, one root per pending promise as the store's
  record is today, and keeps no promise cell alive.
- A cell that died before its resolver needs nothing: its entry's weak
  reference is cleared, and the notice only drops the entry.

This is passive destruction (`passive-managed-destruction-with-drop-records`).
That decision exists because:
- **dying objects are destroyed in no defined order**, so a destructor that
  touched a `Gc` it holds could reach one already destroyed, and one that
  kept a dying edge would resurrect it;
- **destructors run without heap authority**: during collection the
  collector holds the finalizing entry, and at last-owner teardown there is
  no mutator at all, so a destructor that entered the heap, the runtime,
  a scheduler or a callback could deadlock or be unsound, and a panic after
  dispatch poisons the heap;
- **collection is operational only**: its timing never changes Glam
  results, so what a death means semantically happens later, at a safe
  point the runtime chooses, as its drain of external owners after a
  collection does.

Queueing the notice keeps to those reasons. The send is on an unbounded
channel: it never blocks, takes no lock the runtime holds, touches no
`Gc`, and fails harmlessly once the runtime is gone. Breaking the promise
waits for the runtime's safe point after the collection, as it does today.
But it went beyond the letter of the decision as then worded.
`ManagedFamily`'s safety contract (`src/core/managed.rs`) forbids a
destructor to "obtain or invoke" an "event service" or "equivalent active
semantic capability", and says "any exception to passive managed
destruction requires a separate design review". The decision's passive
examples update only an external atomic counter, which means nothing to
Glam. A notice means something: the promise breaks.

This note was that review. The decision's wording forbade more than its
reasons do: they forbid a destructor to act, not to report for the runtime
to act later. The maintainer accepted the clarification (2026-10-23), and
asked that decisions carry their reasons: the decision now records its
reasons and lets a managed destructor queue plain data on a runtime's
notice channel, as do `ManagedFamily`'s contract, `architecture/values.md`
"Managed Families" and the evaluation context. The breaker's destruction
record cites it.

Alternatives:
- **An external-owner token held by the breaker**, whose destructor would
  queue the notice once the registry drains it. An earlier draft of this
  note proposed it. It keeps to the letter of the decision, since the send
  then happens at the registry's drain, but it is the same notice through
  an indirection. It also puts an external owner inside the managed graph,
  where the contract says an external owner "must remain outside".
- **A sweep after each collection** over ephemerons keyed on breakers,
  breaking the cells whose breakers cleared: work in proportion to the
  promises pending at every collection. Rejected, for the same reason as
  the scan.
- **The breaker's destructor breaking the cell itself**: it would touch a
  managed edge and act during collection, against every reason above.

### Resolution, status and transactions

- **Resolving.** `.promise.fulfill`, `.promise.break` and the turn's
  `promise_fulfill`/`promise_break` stage the resolution by the cell's
  `id`, keeping the resolver, and so its breaker, until the transaction
  ends, so a staged resolution never races the breaker's death. At
  commit, the breaker's status settles, the weak table's entry goes, and
  the value is published into the cell if it still lives; if it does not,
  no one could read the value, and nothing is published.
- **A second resolution** conflicts or fails as now, judged by the
  breaker's status and the journal's staged status.
- **Status** reads the breaker's status, or the journal's staged status
  within a transaction that resolves it. A read in a transaction observes
  `promise_path(id)`, the cell's `id`, which resolution writes: conflicts
  and wakes work as now.

### `.try` and evaluation

- `.try` answers `wait` when its block rests on a pending promise with no
  producer: a cell resolved from outside, through a resolver. The
  coordinator already gives the awaited cell's root; the cell has its
  `id`, whether it is assigned, and whether it has a producer. No store
  lookup.
- The `wait` observes `promise_path(id)`, as now, so a cut that fails on
  it wakes on its resolution.
- A promise with a producer, internal to evaluation, stays work in
  progress, as now.

### Obligations and settlement

- Acceptance holds the resolver, as `obligations-are-accepted-promises`
  has it, so an accepted promise is never lost.
- Many promises are forgotten, their cells collected with no promised
  value left, as a task's unit result often is. That does not forget an
  obligation (maintainer, 2026-10-23): settlement reports an accepted
  promise still pending, from its breaker's status, whether or not its
  cell lives.

## What changes

- `PromiseCapability` retires: resolvers become `Value::Resolver`, with
  the value-kind work that volumes needed: the value matches, the
  destruction record the new managed family needs, the inventories.
- Resolvers are abstract: pure evaluation cannot compare them. Under
  reflection, a resolver's representation, and so its structural
  equivalence, is its breaker's identity (maintainer, 2026-10-23).
- `Promises` shrinks to the weak table and the notice channel: no roots, no
  counter of its own, no `pending_with_cell`.
- `StoreEdit::Resolve` names the cell's `id` and keeps the resolver.
- The decisions it revises: `opaque-resources-reclaimed-when-unreachable`
  ("resolvers stay opaque payloads"; "a resolver may keep its promise
  alive"), by an ADR, `promise-cells-are-weakly-resolved`, once built.
- The decision it clarified, already recorded:
  `passive-managed-destruction-with-drop-records`, with its reasons.

## Steps

1. `api-promise-cells-review` — the maintainer reviews this note
   (reviewed 2026-10-23; see Answers).
2. `api-promise-cells` — the build:
   - breakers and `Value::Resolver`, with the breaker's notice and the
     weak table;
   - resolution, status and the death notices on the cell's `id`;
   - `.try` reading the awaited cell, and `pending_with_cell` gone;
   - tests: a lost resolver breaks its promise; a resolver outlives its
     cell, and resolving it does nothing while its status answers; a
     pending promise whose value no one holds is reclaimed; a cut that
     resolves holds its breaker until it ends; `.try` answers `wait` on a
     resolver's promise and waits out a producer's.

## Answers (maintainer, 2026-10-23)

1. Status per breaker. A `'forgotten` status, for a cell no promised value
   holds, waits for a use case: its readers would need waking when the
   cell dies, as for a virtual resource.
2. The breaker is a managed object, not an opaque payload, and serves as
   its own notice source, by the clarification of passive destruction
   above, accepted.
3. This comes before `api-refl-child-tasks`.
4. A forgotten promise is not a forgotten obligation.
5. Resolvers are abstract; their equality is seen only under reflection.
6. A breaker over an opaque resolver is a performance choice; see "A
   breaker or an opaque resolver".
