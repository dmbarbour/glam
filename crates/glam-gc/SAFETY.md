# `glam-gc` Safety Ledger

This file is the authoritative inventory of unsafe code and collector safety
invariants for the Glam-owned garbage collector. Update it in the same change
which adds or changes an unsafe operation.

## Phase C0 Status

The crate contains no unsafe blocks, unsafe functions, or unsafe trait
implementations. `scripts/audit-unsafe.sh` latches that baseline by compiling
the crate with `unsafe_code` forbidden.

The only implemented collector-shaped operation is entry into an empty heap.
It creates a scoped, non-`Send`, non-`Sync` `Mutator` token. There are no managed
pointers, allocations, roots, callbacks, destructors, collector phases, or
shared mutable collector fields.

## Future Unsafe Inventory

For every unsafe module or operation, record:

- source path and enclosing item;
- why safe Rust is insufficient;
- the caller obligations;
- the local proof which discharges those obligations;
- aliasing, initialization, provenance, thread, and panic assumptions;
- the tests, Miri cases, Loom model, or sanitizer run which exercise it; and
- review status.

## Governing Invariants

The cross-plan invariants are maintained in
[`../../docs/plans/GarbageCollectionRoadmap_2026-08-19.md`](../../docs/plans/GarbageCollectionRoadmap_2026-08-19.md).
This ledger will restate only the concrete representation facts needed to audit
unsafe code; it will not create competing semantic rules.
