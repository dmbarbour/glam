# Explicit Heap Holds — 2026-10-09

Status: planned, not started (maintainer, 2026-10-09: a widespread task, to
follow lower-hanging work). Steps, in order: `gc-hold-api`,
`gc-hold-boundaries`, `gc-hold-tests`, `gc-hold-class-cache`, then
`gc-hold-thread-context`.

## Purpose

Separate holding a heap from accessing it, so that admission, the one step
that can block on a running collection, happens only at explicit,
enumerable boundaries. Today any `with_mutator` deep in a call stack may
become an outer entry, which is what made the lock-order audit for
`gc-bounded-collection-wait` hard. This plan replaces
`gc-two-level-mutator-access` and `glam-gc`'s part of
`gc-thread-local-heap-context` from the
[structural overheads plan](StructuralOverheads_2026-10-08.md).

## Design

Agreed with the maintainer on 2026-10-09:
- **A hold is the only admission.** It is explicit and taken at
  boundaries, and a thread holds at most one; a second hold panics.
- **Access requires a hold**, and panics without one in every build. The
  access token, the phantom-scoped mutator, derives from the thread-local
  hold state. A nested access returns another scope proof; accesses are
  counted.
- **`Hold::suspend` is a scoped GC safepoint.** It is valid only with no
  access open; it releases admission and re-acquires it after. It belongs
  where the thread holds no Glam lock, so above the wait functions rather
  than inside `CountedCondvar`, which instead asserts that no hold is
  active while it waits.
- **Holds cache allocation classes.** Class discovery on each access is
  about 4% of `countdown_800` (metadata lookup, `RunGeometry::derive`,
  `discover_class_with`).

Held regions (`perf-quantum-region`) are the current approximation: an
evaluation quantum holds the heap without a token, accesses inside it are
recursive entries, and release points end the hold without re-acquiring it.

## Measurements

Call sites that entered the heap with no hold, from the library tests at
`4072eb92` (the integration tests did not finish within the run's time
limit, so the counts are a floor):

| Kind | Sites | Natural hold point |
| --- | ---: | --- |
| Public host API: `Values` (about 32 methods), `Assembler`, evaluator, diagnostics | about 50 | each API method |
| Compile and setup: module lowering, source parsing, compiler values, macro runner | about 8 | the compile entry |
| Reflection polls: machine, protocol, requests, lifecycle | about 9 | the poll, suspended around client callbacks |
| Polls driven directly by a caller (`whnf`, session, list and access machines) | about 6 | their drivers |
| Tests, mostly through `eval/test_support`, lazy-checkpoint and `whnf` fixtures | about 290 | the test, through the helpers |

Nesting today: production nests an access inside an access only through
the public value API (`Values::list` and `record` run the caller's
iterator inside their own access; diagnostic emission) and cycle poisoning
(`pump.rs`, rooting a failure through a second access). Forbidding nesting
outright broke 186 tests.

## Steps

### Hold and access API (`gc-hold-api`)

`glam-gc` gains `Heap::hold`, where a nested hold panics, `Hold::suspend`,
and access that requires a hold, with nested accesses as plain scope
proofs. A temporary shim keeps today's automatic entry for unconverted
callers.

### Boundaries (`gc-hold-boundaries`)

Convert Glam's boundaries: the public API, the compile entry, reflection
polls (hold their pure steps, such as local state, and suspend around
client callbacks), and directly driven polls. Turn release points into
suspend scopes above the locks: the coordinator's change wait, the client
demand result wait, the runtime activity wait, the net disturbance wait,
the reflection lifecycle waits, host calls, launchers, nested drivers and
pressure servicing. Fix the production nesting sites: collect a caller's
iterator before taking access, and reuse an open access in diagnostics
and cycle poisoning.

### Tests and enforcement (`gc-hold-tests`)

Convert the test helpers, then remove the shim, so unheld access panics in
every build.

### Class cache (`gc-hold-class-cache`)

Cache allocation classes in the hold, so an access finds its class without
the registry, the geometry derivation or the heap's data mutex. A smaller
per-thread cache may land earlier under `perf-allocation-path`; this step
moves it into the hold.

### Heap context in Glam (`gc-hold-thread-context`)

With one heap per thread, Glam's call chains could find their runtime from
the hold instead of passing value factories down (maintainer,
2026-10-09). Investigate the trade: a thread-local lookup against passed
references, and which same-runtime validations it makes redundant.
