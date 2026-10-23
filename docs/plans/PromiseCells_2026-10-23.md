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
  value), the cell's `id`, its own status record, and the token below.
  Its status starts pending and becomes fulfilled or broken when its
  resolution commits, so status always answers from the resolver's side,
  whether or not the cell still lives (maintainer, 2026-10-23: status per
  breaker; a `'forgotten` status, for a cell no promised value holds,
  waits for a use case, since it would need the cell's death to wake its
  readers).
- **Resolver.** A value kind, `Value::Resolver`, an edge to the breaker,
  as volume handles are edges to volume cells (`api-volume-cells`).
  Copies of a resolver share one breaker.

### A lost resolver

Managed destructors stay passive, and active cleanup lives in external-owner
RAII (`passive-managed-destruction-with-drop-records`), so the breaker
cannot break its cell as it dies. The breaker holds a **token**, an
edge-free external owner carrying the cell's `id` and the notice channel.
When the breaker dies, its token is released, and the token's destructor
queues `DeathNotice::Resolver(id)`, as the capability's does today. After
a collection, the runtime finds the cell through a **weak table**, the
cell's `id` to a weak reference to it, and breaks it with "resolver lost",
with a commit as today.

- The table is keyed by the cell's own `id`: a lookup, never a scan.
- An entry goes when its promise resolves, or when its notice arrives.
  It holds the cell weakly, so the table keeps nothing alive.
- A cell that died before its resolver needs nothing: its entry's weak
  reference is cleared, and the notice only drops the entry.

Alternatives:
- **A sweep after each collection** over ephemerons keyed on breakers,
  breaking the cells whose breakers cleared: no token, but work in
  proportion to the promises pending at every collection. Rejected, for
  the same reason as the scan.
- **The breaker's destructor** breaking the cell: an active managed
  finalizer, rejected by `passive-managed-destruction-with-drop-records`.

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
- A promise no one holds a promised value of has no one waiting on it: its
  breaker's weak reference is cleared. Settlement may then count it as
  owed by its acceptor but not awaited, rather than unresolved, which
  fits "a request no one answered is unresolved only when something waits
  on its reply". To decide in `api-settlement`.

## What changes

- `PromiseCapability`'s payload becomes the breaker's token; resolvers
  become `Value::Resolver`, with the value-kind work that volumes needed:
  the value matches, the destruction record the new managed family needs,
  the inventories.
- `Promises` shrinks to the weak table and the notice channel: no roots, no
  counter of its own, no `pending_with_cell`.
- `StoreEdit::Resolve` names the cell's `id` and keeps the resolver.
- The decisions it revises: `opaque-resources-reclaimed-when-unreachable`
  ("resolvers stay opaque payloads"; "a resolver may keep its promise
  alive"), by an ADR, `promise-cells-are-weakly-resolved`, once built.

## Steps

1. `api-promise-cells-review` — the maintainer reviews this note.
2. `api-promise-cells` — the build:
   - breakers and `Value::Resolver`, with the token and the weak table;
   - resolution, status and the death notices on the cell's `id`;
   - `.try` reading the awaited cell, and `pending_with_cell` gone;
   - tests: a lost resolver breaks its promise; a resolver outlives its
     cell, and resolving it does nothing while its status answers; a
     pending promise whose value no one holds is reclaimed; a cut that
     resolves holds its breaker until it ends; `.try` answers `wait` on a
     resolver's promise and waits out a producer's.

## Open questions

1. Settlement's treatment of an accepted promise no one awaits, above.
2. Whether a resolver's equality is its breaker's identity, as a volume
   handle's is its cell's.
