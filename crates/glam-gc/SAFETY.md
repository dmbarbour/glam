# `glam-gc` Safety Ledger

This file is the authoritative inventory of unsafe code and collector safety
invariants for the Glam-owned garbage collector. Update it in the same change
which adds or changes an unsafe operation.

## Implemented Phase Status

C1A introduced a pointer-only `Gc<T>` and a deliberately leaking prototype
allocation path. C1B adds the visitor-based `Trace` contract and representative
manual implementations. C1C adds the structural edge-replacement gateway and
freezes this boundary for the initial non-moving collector. C2A.1 adds pure
fixed-run geometry, and C2A.2 adds heap-owned aligned arena chunks plus checked
numeric owner recovery. C2A.3 initializes integer-only run headers and
allocation, lease, and mark side metadata, but still adds no payload
allocation. C2B.1 adds canonical process-wide object metadata and erased
trace/drop dispatch, but no heap-local allocation class or typed run. There is
C2B.2 adds heap-local dense allocation classes and typed class handles. C2B.3
publishes typed runs, authoritative class pools, and checked metadata
resolution, but no payload allocation. There is no root registry, collection,
marking, reclamation, finalization, callback, or collector coordination.

The crate denies unsafe code by default. `src/lib.rs` gives the reviewed
`pointer`, `mutator`, `trace`, `mutation`, and unit-test modules named lint
expectations for unsafe code. The exact module expectations and every unsafe
function, implementation, and block are checked into
`scripts/unsafe-modules.txt` and `scripts/unsafe-sites.txt`;
`scripts/audit-unsafe.sh` fails when either inventory changes.

## Arena Ownership Invariants

- Every arena chunk is one 8 MiB allocation with size and alignment equal to
  8 MiB. It therefore contains exactly 128 aligned 64 KiB runs.
- `HeapInner` owns its arena behind the heap mutex. A chunk is never published
  until allocation and overlap validation both succeed; dropping either a
  rejected candidate or its arena returns the allocation exactly once.
- Owner lookup first compares an integer address with live chunk ranges. Only
  a successful range check derives a run pointer from the original chunk
  pointer, preserving allocation provenance without dereferencing a guessed
  header.
- Live chunks are required not to overlap. Run recovery masks a validated
  address by the fixed run size, and checked pointer arithmetic remains inside
  the owning chunk.
- Arena bytes are zeroed but untyped in C2A.2. No `RunHeader`, payload, bitmap,
  or Rust reference exists in them at that checkpoint. C2A.3 initializes every
  run header before chunk publication, then initializes side metadata only
  while the arena is exclusively borrowed.

## Run Topology Invariants

- `RunHeader` is an integer-only, 64-byte, 64-byte-aligned representation at
  the start of every run. All bit patterns are valid Rust values, and the
  header magic distinguishes initialized collector topology from corruption.
- Every newly reserved chunk initializes all 128 empty headers before the
  chunk enters its arena. A run becomes class-owned only after its geometry is
  structurally revalidated and its three disjoint side-bitmap ranges are
  cleared.
- Failed geometry, missing-run, invalid-header, and repeated-initialization
  paths publish no new class identity. Initialization contains no payload
  write and no operation which can fail after bitmap clearing begins.
- The header stores a nonzero 64-bit heap-local dense allocation-class identity
  and checked slot/bitmap geometry. Canonical type metadata remains in the
  heap's dense class table rather than consuming payload or header bytes.
- Checked slot-owner recovery first finds the owning live chunk and run, then
  reads its already initialized header, validates its class and reconstructed
  geometry, and accepts only an exact slot-start address. Header bytes,
  metadata, alignment padding, slot interiors, run ends, free runs, and other
  heaps all fail without producing an owner.

## Prototype Representation Invariants

- `Gc<T>` is transparent over exactly one `NonNull<T>`. An unconditional const
  assertion latches its one-pointer width. It carries no heap, domain, class,
  allocation-record, or debug field and is not a root. It supports pointer-
  identity equality but deliberately does not implement `Hash`, because an
  address hash could not remain stable across later moving collection.
- `Mutator<'heap>` contains a reference to exactly one `HeapInner`. Its
  `Rc<()>` phantom makes the token neither `Send` nor `Sync`; the lifetime
  prevents it from outliving that heap entry.
- `Mutator::alloc` accepts only `T: Trace`. Its inline const assertion rejects
  zero-sized types while compiling an invalid monomorphization, before any
  allocation can run. It initializes and leaks a `Box<T>`, registers its
  address, and only then constructs `Gc<T>`.
- Prototype payloads never move and are never destroyed. The allocation record
  is diagnostic metadata and may disappear with its heap; no access is valid
  without a live mutator for the owning heap.
- In debug/test builds, each heap records an allocation address and canonical
  object-metadata pointer under one mutex. Access copies the matching record
  and releases the mutex before asserting heap ownership or metadata identity,
  so a deliberately caught contract panic does not poison the registry.
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

### Canonical object metadata and erased dispatch

Cold discovery constructs immutable metadata outside the process registry
mutex, then publishes exactly one process-lifetime descriptor for each
`TypeId`. A losing candidate remains an ordinary `Box` and is dropped; an
injected construction panic occurs before the registry is reacquired. Only the
winning descriptor is leaked intentionally. After discovery, its static
address is the operational representation identity.

Each descriptor records the exact Rust `Layout`, the representation's one
optional requested total slot extent, monomorphized erased trace dispatch, and
either no destructor or the monomorphized erased destructor. The request is
the desired complete slot size before alignment rounding, not additional
padding; the resulting stride may be larger. Generic const evaluation rejects
a request smaller than `size_of::<T>()` before metadata can be constructed.
The request is an associated constant of the unsafe `Trace` implementation, so
a different policy requires a distinct Rust wrapper type and `TypeId`.

Erased trace and drop calls cast only after the caller proves that the pointer
names a live initialized allocation whose run resolves to that exact metadata.
The trace dispatcher forms a shared `T` reference only for the duration of the
synchronous visit. The drop dispatcher invokes `drop_in_place::<T>` exactly
once and does not deallocate the slot. C2B.1 tests these functions with
prototype allocations; C2B.3 makes typed-run metadata resolution the ordinary
proof source, and C6 owns actual destruction.

## Heap-Local Allocation-Class Invariants

- Canonical metadata discovery and pure fixed-run geometry derivation finish
  before the heap-state mutex is acquired. Unsupported layouts therefore
  publish no heap-local state or dense ID.
- One heap-state mutex guards the arena, metadata-identity index, and dense
  class table. Metadata keys compare and hash only the canonical static
  descriptor address; `TypeId` is absent from this state.
- Immutable class candidates are constructed outside the heap lock. After the
  second winner check, vector and map capacity is reserved before either
  authoritative structure is changed, then the dense entry and its index are
  published under the same mutex.
- `AllocationClass<T>` retains its `HeapInner`, exact metadata address, and
  dense class ID. The heap state contains class entries, not handles, so this
  strong provenance capability creates no ownership cycle. It retains no
  managed allocation and is not a root.
- A class handle is constructible only inside this crate after metadata and
  geometry agreement. C2C's safe allocator must compare its retained heap with
  the mutator heap in release builds before accessing run state.

## Typed-Run Publication and Resolution Invariants

- One class entry owns a directly enumerable vector of every `RunLocation`
  published for it. Thread-local cursors and later range leases do not own or
  retain the run and are not required for enumeration.
- Before arena publication, the class run vector reserves its next entry.
  Arena publication first reuses an existing empty run, or allocates and fully
  initializes a candidate chunk's first run before the chunk enters the arena.
  Failed allocation, overlap validation, geometry validation, or injected
  publication failure therefore adds neither a typed run nor a class-pool
  location.
- After a successful arena publication, recording the already reserved
  `RunLocation` is infallible and occurs under the same heap-state mutex. No
  observer can see a header without its class-pool membership.
- Checked slot resolution validates arena membership, exact slot-start
  geometry, dense class-table membership, class geometry, and authoritative
  run-pool membership before returning canonical metadata. A header ID from
  another heap has no meaning outside its owner state.
- `AllocationClass<T>` provenance is checked before the heap mutex and before
  any run state is changed. A foreign handle therefore cannot allocate or
  publish a run even when its numeric dense ID happens to equal a local ID.

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

### `arena::ArenaChunk` allocation and destruction

`alloc_zeroed` receives one checked nonzero `Layout` whose size and alignment
are both the fixed arena-chunk size. A null result becomes `AllocationFailed`
before any chunk is published. The successful pointer remains uniquely owned
by its `ArenaChunk`, whose `Drop` returns it through `dealloc` with the identical
layout.

The two `NonNull::add` sites derive aligned run starts. Both are preceded by
either a checked run index or numeric membership and mask validation, proving
the result remains within the live chunk. Neither site dereferences or creates
a Rust reference.

### `Send for arena::ArenaChunk`

An arena chunk owns raw untyped bytes and exposes no Rust reference. Moving the
owner between threads transfers the one deallocation obligation without
accessing those bytes. Sharing remains mediated by the heap's arena mutex; no
independent `Sync` implementation is needed.

### Run-header and side-metadata access

Fresh-chunk setup derives each disjoint aligned run start and writes one valid
`RunHeader::empty` before publishing the chunk. Checked lookup forms a shared
header reference only after numeric chunk membership and run-alignment
validation; the reference remains bounded by the arena borrow.

Run initialization takes an exclusive arena borrow. It reads the existing
integer-only header, validates all geometry before pointer arithmetic, clears
the allocation, lease, and mark byte ranges, and then overwrites the empty
header with the initialized representation. The test-only slice construction
copies those same already initialized metadata bytes without returning a
borrow. No unsafe site touches payload storage.

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
  the collector rejects zero-sized payloads; its implementation exists so the
  compile-fail fixture can instantiate the generic allocation boundary.
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
  mutator region, a mutator cannot enter a scoped worker, zero-sized managed
  allocation is rejected by const evaluation, `Gc<T>` has no `Deref` path, and
  address identity does not implement `Hash`.
- Unconditional const assertions enforce the `Gc<T>` pointer-width contract.
  Unit tests cover pointer identity, cross-thread handle transfer, nested
  separate heaps, debug rejection of wrong heap and representation, exact
  recursive edge sequences with duplicate pointers, full retracing after an
  injected visitor panic, exact edge replacement, and rejection before
  foreign-heap mutation.
- Arena tests cover first, last, and adjacent run boundaries, live chunk
  non-aliasing, separate-arena and separate-heap ownership, and mask arithmetic
  at the highest representable complete chunk range.
- Run-topology tests cover every empty header, independent adjacent class
  identities and geometry, zeroed bitmap ranges, exact first and last slots,
  rejection of non-slot addresses, and non-publication after invalid or
  repeated initialization.
- The ordinary crate checks, exact unsafe inventory, focused Miri run, and
  repository-wide checks are required for completed C1.
- Miri passes all implemented tests with `-Zmiri-ignore-leaks`. That flag suppresses only
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
