# `glam-gc` Safety Ledger

This file is the authoritative inventory of unsafe code and collector safety
invariants for the Glam-owned garbage collector. Update it in the same change
which adds or changes an unsafe operation.

## Phase C1 Status

C1A introduced a pointer-only `Gc<T>` and a deliberately leaking prototype
allocation path. C1B adds the visitor-based `Trace` contract and representative
manual implementations. C1C adds the structural edge-replacement gateway and
freezes this boundary for the initial non-moving collector. There is still no
root registry, collection, marking, reclamation, finalization, callback, or
collector coordination.

The crate denies unsafe code by default. `src/lib.rs` gives the reviewed
`pointer`, `mutator`, `trace`, `mutation`, and unit-test modules named lint
expectations for unsafe code. The exact module expectations and every unsafe
function, implementation, and block are checked into
`scripts/unsafe-modules.txt` and `scripts/unsafe-sites.txt`;
`scripts/audit-unsafe.sh` fails when either inventory changes.

## Prototype Representation Invariants

- `Gc<T>` contains exactly one `NonNull<T>`. It carries no heap, domain, class,
  allocation-record, or debug field and is not a root. It supports pointer-
  identity equality but deliberately does not implement `Hash`, because an
  address hash could not remain stable across later moving collection.
- `Mutator<'heap>` contains a reference to exactly one `HeapInner`. Its
  `Rc<()>` phantom makes the token neither `Send` nor `Sync`; the lifetime
  prevents it from outliving that heap entry.
- `Mutator::alloc` accepts only `T: Trace` and rejects zero-sized types before
  allocation. It initializes and leaks a `Box<T>`, registers its address, and
  only then constructs `Gc<T>`.
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
restricted to `T: Trace`, which itself requires `Send + Sync`. Copying or
sharing `Gc<T>` does not grant access: dereference still requires a non-`Send`,
non-`Sync`, heap-qualified mutator. `T: Sync` permits the resulting shared
reference on another thread; `T: Send` permits eventual collector-thread
destruction. The cross-thread prototype test moves a `Gc<u64>` and accesses it
only after entering its owner heap on that thread.

### `mutator::Mutator::alloc`

The safe prototype allocator calls `Gc::from_raw` in one unsafe block. Its
local proof is the initialization, leak, pointer derivation, and registration
sequence described above. A panic before pointer return can leak more memory
but cannot expose an invalid handle.

### Test call sites

The remaining inventoried unsafe blocks call `get_unchecked` or
`with_edge_replacement` from focused tests. Correct-access sites state their
heap, liveness, representation, and replacement proofs inline. Mismatch sites
run only with debug assertions and test that the relevant gateway panics
before dereference or mutation. No test creates a Rust reference with an
invalid type or provenance.

### `trace::Trace`

`Trace` is unsafe because omitting one managed edge can make a later exact
collector reclaim a reachable allocation. Implementations must synchronously
visit every `Gc<_>` represented by the value, must not invent invalid edges or
retain the visitor, and must not hide a managed pointer in another
representation. Reporting a valid edge more than once is permitted.

Tracing is observational. An implementation may inspect immediate fields to
select edges but may not mutate the managed graph, collector metadata, or
interior state in a way that changes a later trace. If either the
implementation or visitor panics, the value remains valid and can be traced
again from the beginning. C1B invokes no allocation, callback, destruction, or
reclamation through this interface.

### Structural `Trace` implementations

- `Gc<T>` reports its one managed pointer.
- `Option<T>` reports exactly its present payload, if any.
- `[T; N]` visits every element once in index order.
- `(First, Second)` visits both fields in order.
- `()`, `u32`, and `u64` report no edges. Unit remains unallocatable because
  the collector rejects zero-sized payloads; its implementation exists only so
  the C1A rejection fixture can instantiate the generic allocation boundary.
- The test-only `Leaf`, recursive `Node`, and `RepresentativeStruct` manually
  report all their declared managed fields. They exercise the same contract but
  do not become collector API.

These are the complete admitted implementations for C1B. In particular, there
is no generic container, slice, persistent-collection, derive, raw-pointer, or
opaque-value escape hatch.

### Visitor erasure boundary

`Visitor::visit` converts `Gc<T>` to a private pointer-only `ErasedGc` and
synchronously invokes its collector-owned receiver. Erasure preserves the
managed address but neither constructs a reference nor adds or guesses heap or
type metadata. Later typed-run lookup must recover and validate those facts.
The receiver may panic; no visitor or traversal state is stored in the traced
object.

### `mutation::Mutator::with_edge_replacement`

The raw mutation gateway is named for the operation it encloses rather than an
edge write it performs. It reports the owner, old edge, and new edge to the
collector, then invokes the caller's closure; the closure performs the actual
storage mutation. The gateway is unsafe because pointer-only `Gc<T>` does not
carry release-visible heap provenance and because the collector cannot infer
which representation slot the closure changes. The caller must prove that
owner, old edge, and new edge are live allocations in the mutator heap; that
old describes the slot before the closure; and that new describes it if the
closure returns. The closure performs one logical replacement and leaves the
containing representation valid even if it panics after mutation.

Debug/test builds validate every supplied pointer before running the closure.
The separate always-inlined collector hook receives erased owner/old/new
pointers and is empty for the initial stop-the-world collector. Therefore it
adds no optimized collector action today while preserving one auditable site
for a later Dijkstra-, SATB-, or generation-specific barrier. A future barrier
may conservatively retain both old and new edges if the mutation closure
panics.

The mutation fixtures access a managed `Mutex<Option<Gc<_>>>` only under its
owner mutator. Its manual trace snapshots and releases the mutex before calling
the visitor, so visitor panic neither poisons the mutex nor changes the graph's
retraceability. The foreign-heap test proves debug rejection occurs before its
mutation closure runs.

## Verification and Review Status

- Compile-fail doctests prove that a borrowed managed value cannot escape its
  mutator region, a mutator cannot enter a scoped worker, `Gc<T>` has no
  `Deref` path, and address identity does not implement `Hash`.
- Unit tests cover pointer size, pointer identity, zero-sized-type rejection,
  cross-thread handle transfer, nested separate heaps, debug rejection of
  wrong heap and representation, exact recursive edge sequences with duplicate
  pointers, full retracing after an injected visitor panic, exact edge
  replacement, and rejection before foreign-heap mutation.
- The ordinary crate checks, exact unsafe inventory, focused Miri run, and
  repository-wide checks are required for completed C1.
- Miri passes all C1 tests with `-Zmiri-ignore-leaks`. That flag suppresses only
  the prototype allocator's deliberate `Box::leak`; C2 must remove it when it
  introduces arena ownership.
- The C1 pointer/access/trace/mutation surface is reviewed and frozen for the
  initial non-moving collector. A moving collector may add a distinct edge-
  rewriting contract; this observational visitor does not promise relocation.

## Governing Invariants

The cross-plan invariants are maintained in
[`../../docs/plans/GarbageCollectionRoadmap_2026-08-19.md`](../../docs/plans/GarbageCollectionRoadmap_2026-08-19.md).
This ledger restates only the concrete representation facts needed to audit
unsafe code; it does not create competing semantic rules.
