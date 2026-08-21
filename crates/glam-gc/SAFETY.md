# `glam-gc` Safety Ledger

This file is the authoritative inventory of unsafe code and collector safety
invariants for the Glam-owned garbage collector. Update it in the same change
which adds or changes an unsafe operation.

## Phase C1A Status

C1A introduces a pointer-only `Gc<T>` and a deliberately leaking prototype
allocation path. There is still no tracing, root registry, collection,
reclamation, finalization, callback, or collector coordination.

The crate denies unsafe code by default. `src/lib.rs` gives the reviewed
`pointer`, `mutator`, and unit-test modules named lint expectations for unsafe
code. The exact module expectations and every unsafe function, implementation,
and block are checked into
`scripts/unsafe-modules.txt` and `scripts/unsafe-sites.txt`;
`scripts/audit-unsafe.sh` fails when either inventory changes.

## Prototype Representation Invariants

- `Gc<T>` contains exactly one `NonNull<T>`. It carries no heap, domain, class,
  allocation-record, or debug field and is not a root.
- `Mutator<'heap>` contains a reference to exactly one `HeapInner`. Its
  `Rc<()>` phantom makes the token neither `Send` nor `Sync`; the lifetime
  prevents it from outliving that heap entry.
- `Mutator::alloc` accepts only `T: Send + Sync + 'static`. It initializes a
  boxed `PrototypeSlot<T>`, leaks the box, obtains a pointer to its `T` field,
  registers that address, and only then constructs `Gc<T>`. The slot's extra
  byte ensures that zero-sized `T` values also have distinct addresses.
- Prototype payloads never move and are never destroyed. The allocation record
  is diagnostic metadata and may disappear with its heap; no access is valid
  without a live mutator for the owning heap.
- In debug/test builds, each heap records allocation address, `TypeId`, and
  type name under one mutex. Access copies the matching record and releases the
  mutex before asserting heap ownership or representation, so a deliberately
  caught contract panic does not poison the registry.
- Converting a pointer to `usize` is used only to compare registry keys. No
  managed pointer or Rust reference is reconstructed from that integer.
- `Gc<T>` exposes only shared access. `T: Sync` makes such access valid across
  threads; the additional `T: Send` bound reserves safe destruction on a
  collector thread. Managed interior mutation will require a later reviewed
  gateway rather than casting this shared reference.

## Unsafe Inventory

### `pointer::Gc::from_raw`

Safe Rust cannot express that an arbitrary non-null pointer is a registered
managed allocation. This crate-private unsafe constructor makes that proof an
explicit allocator obligation.

The caller must prove that the address is non-null, aligned, initialized as
`T`, registered to the allocating heap, and live for the collector-defined
period. C1A's only caller is `Mutator::alloc`, which obtains the pointer from a
fully initialized leaked box and registers the same address and type before the
call. A future allocator or representation decoder must discharge the same
obligations independently.

### `pointer::Gc::get_unchecked` and its pointer dereference

`NonNull::as_ref` requires unsafe code because the pointer itself proves none
of liveness, heap ownership, representation, or alias validity. The caller
must prove that the pointer is a live initialized `T` in the supplied mutator's
heap and that no mutation invalidates the returned shared reference. The
implementation performs the available debug/test heap and `TypeId` checks
before `as_ref`; its result lifetime is bounded by the mutator borrow.

Correct prototype calls satisfy the proof because C1A never reclaims or mutates
payloads. Wrong-heap and wrong-representation tests deliberately arrange a
diagnostic mismatch and establish that the check panics before `as_ref` runs.
Those checks are not part of the release-build proof.

### `Send` and `Sync` for `Gc<T>`

`NonNull<T>` does not grant these auto traits. The unsafe implementations are
restricted to `T: Send + Sync`. Copying or sharing `Gc<T>` does not grant
access: dereference still requires a non-`Send`, non-`Sync`, heap-qualified
mutator. `T: Sync` permits the resulting shared reference on another thread;
`T: Send` permits eventual collector-thread destruction. The cross-thread
prototype test moves a `Gc<u64>` and accesses it only after entering its owner
heap on that thread.

### `mutator::Mutator::alloc`

The safe prototype allocator calls `Gc::from_raw` in one unsafe block. Its
local proof is the initialization, leak, pointer derivation, and registration
sequence described above. A panic before pointer return can leak more memory
but cannot expose an invalid handle.

### Test call sites

The remaining inventoried unsafe blocks call `get_unchecked` from focused
tests. Correct-access sites state their heap, liveness, and representation
proofs inline. The two mismatch sites run only with debug assertions and test
that the gateway panics before dereference. No test creates a Rust reference
with an invalid type or provenance.

## Verification and Review Status

- Compile-fail doctests prove that a borrowed managed value cannot escape its
  mutator region, a mutator cannot enter a scoped worker, and `Gc<T>` has no
  `Deref` path.
- Unit tests cover pointer size, pointer identity, distinct zero-sized
  allocations, cross-thread handle transfer, nested separate heaps, and debug
  rejection of wrong heap and representation.
- The ordinary crate checks, exact unsafe inventory, and the repository-wide
  checks are required for C1A.
- These sites are reviewed for the C1A leaking prototype. C1C performs the
  complete pointer/access/trace audit before allocator work starts.

## Governing Invariants

The cross-plan invariants are maintained in
[`../../docs/plans/GarbageCollectionRoadmap_2026-08-19.md`](../../docs/plans/GarbageCollectionRoadmap_2026-08-19.md).
This ledger restates only the concrete representation facts needed to audit
unsafe code; it does not create competing semantic rules.
