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
trace/drop dispatch, but no heap-local allocation class or typed run. C2B.2
adds heap-local dense allocation classes and typed class handles. C2B.3
publishes typed runs, authoritative class pools, and checked metadata
resolution, but no payload allocation. C2C.1a adds indexed chunk ownership and
constant-cost checked chunk lookup. C2C.1b replaces the leaking prototype with
synchronized arena allocation and provisional terminal payload destruction.
C2C.2 adds weak heap-specific TLS cache identity and lifecycle. C2C.3 leases
disjoint allocation-bitmap words and makes the worker-local cursor the ordinary
allocation path. C2C.4 adds batched leased-capacity pressure, deterministic
pre-initialization unwind verification, and last-owner teardown latches.
C2C.5 replaces the baseline lease scan with atomic hierarchical claiming,
moves pressure to authoritative typed-run publication, adds stable atomic class
frontiers, and removes eager TLS pruning in favor of explicit inert release.
C2C.6 adds forced concurrent exhausted-frontier verification and corrects the
publication proof: initial run topology is observed through the frontier
Release/Acquire pair or heap mutex, not through a load of the separate lease
word. C3A adds the heap-local mutator-admission coordinator, moves allocation-
class discovery behind mutator authority, and orders TLS entry as prepare,
admit, then activate. Its synthetic exclusive phase and Loom models verify
drain, exclusion, and visibility without yet electing a production collector.
There is no root registry, collection, marking, reclamation, finalization,
callback, or production collector election.

The crate denies unsafe code by default. `src/lib.rs` gives the reviewed
`pointer`, `mutator`, `trace`, `mutation`, `thread_cache`, and unit-test modules named lint
expectations for unsafe code. The exact module expectations and every unsafe
function, implementation, and block are checked into
`scripts/unsafe-modules.txt` and `scripts/unsafe-sites.txt`;
`scripts/audit-unsafe.sh` fails when either inventory changes.

## Arena Ownership Invariants

- Every arena chunk is one 8 MiB allocation with size and alignment equal to
  8 MiB. It therefore contains exactly 128 aligned 64 KiB runs.
- `HeapInner` owns its arena behind the heap mutex. The stable owning vector and
  an authoritative map from aligned chunk base to vector index are updated
  together. Both collections reserve capacity before publication; dropping a
  rejected candidate or its arena returns the allocation exactly once.
- Owner lookup masks an integer address to a candidate 8 MiB base and queries
  the owning heap's chunk index. Only a successful index and range check derive
  a run pointer from the original chunk pointer, preserving allocation
  provenance without dereferencing a guessed header.
- Live chunks are required not to overlap. Equal masked bases are the only way
  fixed-size, fixed-alignment chunks can overlap. Run recovery then masks the
  validated address by the fixed run size, and checked pointer arithmetic
  remains inside the owning chunk.
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
- The header and three side bitmaps form one metadata prefix. The first payload
  slot begins at the next 128-byte boundary (or its stricter Rust payload
  alignment), so no metadata byte shares a 128-byte region with payload
  storage. This is an explicit layout isolation grain, not a claim about the
  host's physical cache-line size.
- Checked slot-owner recovery first finds the owning live chunk and run, then
  reads its already initialized header, validates its class and reconstructed
  geometry, and accepts only an exact slot-start address. Header bytes,
  metadata, alignment padding, slot interiors, run ends, free runs, and other
  heaps all fail without producing an owner.

## Managed Pointer and Access Invariants

- `Gc<T>` is transparent over exactly one `NonNull<T>`. An unconditional const
  assertion latches its one-pointer width. It carries no heap, domain, class,
  allocation-record, or debug field and is not a root. It supports pointer-
  identity equality but deliberately does not implement `Hash`, because an
  address hash could not remain stable across later moving collection.
- `Mutator<'heap>` contains a reference to exactly one `HeapInner`. Its
  `Rc<()>` phantom makes the token neither `Send` nor `Sync`; the lifetime
  prevents it from outliving that heap entry.
- `Mutator::alloc` accepts only `T: Trace` and requires a reusable
  `AllocationClass<T>` from the same heap. Its inline const assertion rejects
  zero-sized types while compiling an invalid monomorphization, before any
  allocation can run. A foreign class is rejected in every build before either
  heap's run state changes.
- Before collection exists, arena payloads never move and remain live until
  their heap's terminal teardown. No access is valid without a live mutator for
  the owning heap.
- In debug/test builds, access masks the address into the owning heap's indexed
  chunk set, validates run/slot/class topology, and compares the resolved
  canonical metadata pointer. It intentionally diagnoses ownership, shape, and
  representation rather than concurrently changing allocation liveness.
- Converting a pointer to `usize` is used only for indexed ownership and exact
  slot geometry. A managed pointer is rederived only from the original owning
  chunk pointer after successful numeric validation.
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
period. `Mutator::alloc` obtains the pointer only after its synchronized arena
allocator has initialized the payload and published the exact allocation bit.
A future fast allocator or representation decoder must discharge the same
obligations independently.

### `pointer::Gc::get_unchecked` and its pointer dereference

`NonNull::as_ref` requires unsafe code because the pointer itself proves none
of liveness, heap ownership, representation, or alias validity. The caller
must prove that the pointer is a live initialized `T` in the supplied mutator's
heap and that no mutation invalidates the returned shared reference. The
implementation performs the available debug/test indexed heap and canonical
metadata checks before `as_ref`; its result lifetime is bounded by the mutator
borrow.

Correct C2C calls satisfy the proof because collection remains disabled and
arena payloads stay live until heap teardown. Wrong-heap and
wrong-representation tests deliberately arrange a diagnostic mismatch and
establish that indexed validation panics before `as_ref` runs. Those checks are
not part of the release-build proof.

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
once and does not deallocate the slot. Typed-run metadata resolution is the
ordinary proof source, terminal heap teardown is the provisional caller, and
C6 later owns collector-driven destruction.

## Regional Mutator Admission Invariants

- One heap-state mutex protects the admission phase and active-outer-mutator
  count together with the existing class/run topology. Its sibling condition
  variable supplies wakeups only; every waiter loops and rechecks its complete
  predicate under the mutex.
- `Ordinary` admits outer mutators. `ExclusivePending` denies fresh outer
  admission while admitted mutators drain, and `Exclusive` is published only
  after the active count reaches zero. C3A exposes the latter phases only to
  deterministic internal verification; C3B will connect them to requests and
  collector election.
- Preparing a thread-local heap entry obtains or validates its weak heap-
  qualified record without changing recursive depth. An outer preparation then
  obtains one coordinator obligation before activating its cache. A panic or
  block between preparation and activation therefore leaves the TLS record
  inactive, and the admission token's destructor rolls back the active count.
- Recursive same-heap entry observes nonzero thread-local depth and reuses the
  outer coordinator obligation. It remains available while exclusive work is
  pending so an already-admitted mutator can finish its bounded region. Entry
  into a different heap uses that heap's independent TLS record and admission
  count.
- Entry destruction first decrements recursive depth and makes the outer cache
  quiescent, then retires the coordinator obligation. Consequently, observing
  zero active mutators after acquiring the heap mutex also observes all work
  sequenced before those outer exits. C3A's native forced schedules and Loom
  model latch this visibility edge.
- An admitted mutator borrows the `Heap` handle's existing `Arc<HeapInner>`;
  admission does not clone a shared owner. Allocation-class discovery clones
  that owner only into the reusable class handle which must retain heap
  provenance after the discovery region exits.

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
- A class handle is constructible only through an admitted `Mutator` after
  metadata and geometry agreement. The handle remains reusable after that
  region exits. The safe allocator compares its retained heap with the current
  mutator heap in release builds before accessing run state.

## Typed-Run Publication and Resolution Invariants

- One class entry owns a directly enumerable vector of every `RunLocation`
  published for it. Thread-local cursors and later word leases do not own or
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

## Synchronized Payload Allocation and Terminal Destruction

- The correctness allocator holds the heap-state mutex while selecting a free
  allocation bit, initializing its typed payload, and publishing that bit. The
  payload and bitmap word are disjoint validated run ranges, and no operation
  capable of unwinding occurs between their raw writes.
- Selecting a slot changes no persistent state. An injected unwind after
  selection but before initialization leaves the allocation bit clear and the
  input value is destroyed normally. The already published empty typed run may
  remain available for later allocation.
- Allocation-bit reads and writes use initialized aligned `u64` side metadata
  under heap-state exclusion. The synchronized path may scan run pools and
  slots; C2C.3 replaces this correctness baseline with disjoint worker-local
  bitmap-word leases.
- Terminal `HeapInner::drop` has exclusive ownership. It enumerates every set
  allocation bit, dispatches the owning class's destructor exactly once when
  required, and then lets each arena chunk deallocate exactly once. This path
  is provisionally non-reentrant and non-panicking. C2C.4 proves it cannot race
  an active owner region; C6 replaces terminal enumeration with collector-
  controlled finalization, quarantine, and destructor-panic recovery.

## Thread-Local Allocation Cache Lifecycle

- Each thread's registry holds only a weak heap identity, a nonzero captured
  allocation-lease epoch, recursive depth, and a fixed 64-entry direct-mapped
  cursor array. The heap has no cache registry or TLS back-reference.
- The weak Arc identity prevents its allocation address from being reused while
  a stale TLS record exists without keeping the dead heap alive. Ordinary heap
  entry never scans or prunes unrelated records. Thread exit drops the complete
  registry, while `Heap::release_current_thread_caches` validates that every
  recursive depth is zero before clearing all records without heap access.
- Same-heap recursive regions share one TLS entry and checked depth. An RAII
  entry guard balances normal return and unwinding. Different heaps use
  independent records even when their mutator regions are nested on one host
  thread.
- Only outer entry compares the heap's current lease epoch. Mismatch clears the
  entire cursor array and captures the new epoch. Clearing, collision eviction,
  ordinary exit, and TLS destruction do not dereference runs, return leases, or
  mutate heap state.
- The cache's fixed 64 logical entries use one boxed slice so cache width does
  not inflate a mutator caller's stack. Dense class ID selects the entry and
  the retained full ID detects collisions; no hash or allocation occurs on a
  lookup.
- A cursor becomes authoritative only after one atomic run-side lease-bit CAS
  succeeds. C2C leases one complete allocation word per cursor. Separate
  cursors never own the same word, and eviction, explicit cache release, or TLS
  destruction forgets the pointer without clearing its lease.

## Worker-Local Allocation-Word Invariants

- A cache miss first loads the allocation class's stable frontier with Acquire
  ordering and scans its atomic lease bitmap without heap state. Only an
  exhausted frontier enters the synchronized slow path, where exact class
  provenance is revalidated and the frontier advances to an existing run or a
  newly published typed run. A fresh run is fully published into the arena,
  class pool, and run-pressure state before its frontier pointer is stored with
  Release ordering.
- Initial run topology reaches a lock-free claimant through that frontier
  Release/Acquire pair. A claimant reached from the synchronized path instead
  relies on the heap mutex. The separate lease-word Acquire does not publish
  the run record; in C2C it participates in atomic word ownership. C5 will add
  a Release reset on that same lease word to publish rebuilt post-collection
  allocation/free state to the next Acquire claimant.
- The cursor carries the stable owning `RunAddress`, validated `RunGeometry`,
  one allocation-word index, and a local free mask. Its mask is the inverse of
  the authoritative allocation word intersected with the exact slot-count
  mask, so tail padding can never become a payload address.
- While a lease is live, only its worker-local cursor reads or writes that
  allocation word. Other claims inspect `AtomicU64` lease words and read an
  ordinary allocation word only after winning its lease bit. Distinct word-
  sized `u64` objects occupy disjoint memory, so concurrent allocation within
  one run is data-race-free. Lease-word size and alignment suitability are
  compile-time assertions; raw lease storage is initialized directly as
  `AtomicU64`, not reinterpreted from a live `u64`.
- The hot path performs every bounds, size, alignment, and free-bit assertion
  before initializing the payload. Its two final operations are an infallible
  payload write followed by an infallible allocation-bit write; no unwind can
  expose uninitialized storage as allocated. It updates the local free mask
  only after both writes.
- Allocation visibility is not delayed until mutator exit. Returning `Gc<T>`
  occurs only after bitmap publication, and ordinary Rust synchronization may
  immediately share it with another mutator. Collection remains disabled in
  C2C, so the payload then stays live through terminal heap teardown.
- Allocation pressure is charged exactly once under heap state after a typed
  run becomes authoritative in both arena and class pool. Word claims, local
  object allocation, cursor eviction, explicit cache release, and thread exit
  do not touch it. A saturating count latches the provisional request after
  `RUNS_PER_CHUNK` publications, currently 128; C3B owns acting on the request
  and C6 owns rearming the allowance after successful sweep.
- The deterministic pre-initialization hook exists only in test builds at the
  last point before the two publication writes. An unwind there owns and drops
  the input normally while leaving both local and authoritative bit state
  unchanged. Production has no callback at that boundary.

### `Send` and `Sync` for `Gc<T>`

`NonNull<T>` does not grant these auto traits. The unsafe implementations are
restricted to `T: Trace`, which itself requires `Send + Sync`. Copying or
sharing `Gc<T>` does not grant access: dereference still requires a non-`Send`,
non-`Sync`, heap-qualified mutator. `T: Sync` permits the resulting shared
reference on another thread; `T: Send` permits eventual collector-thread
destruction. The cross-thread test moves a `Gc<u64>` and accesses it
only after entering its owner heap on that thread.

### `mutator::Mutator::alloc`

The safe allocator first attempts the worker-local cursor and then the class's
atomic run frontier. Only frontier exhaustion enters the synchronized topology
slow path. Every successful branch calls `Gc::from_raw` only after the exact
payload has been initialized and its allocation bit published. The class-
provenance check precedes allocation, and neither payload path contains a
panicking operation between the payload write and bit publication.

### `thread_cache::AllocationCursor` raw run and bitmap access

The cursor's raw pointer arithmetic, payload initialization, and bitmap access
are justified by the atomic lease operation over a published stable run record:
it carries validated typed-run geometry, masks invalid tail bits, and grants
this thread exclusive ownership of one represented allocation word. The cursor
checks the selected slot, payload size, and alignment before writing. No other
allocator may read or mutate its allocation word until a future full collection
revokes all leases after stopping mutators and advancing the heap epoch.

### `arena::ArenaChunk` allocation and destruction

`alloc_zeroed` receives one checked nonzero `Layout` whose size and alignment
are both the fixed arena-chunk size. A null result becomes `AllocationFailed`
before any chunk is published. The successful pointer remains uniquely owned
by its `ArenaChunk`, whose `Drop` returns it through `dealloc` with the identical
layout.

The `NonNull::add` sites derive aligned run starts, side-metadata words, or
bounded payload slots. Each is preceded by checked run membership and validated
geometry, proving the result remains within the live chunk. Payload and
ordinary bitmap writes occur only under exclusive arena or leased-word access.
Lease words are the exception: after run publication they are accessed only
through `AtomicU64`. Read-only recovery does not create a payload reference.

### `Send for arena::ArenaChunk`

An arena chunk owns raw untyped bytes and exposes no Rust reference. Moving the
owner between threads transfers the one deallocation obligation without
accessing those bytes. Sharing remains mediated by the heap's arena mutex; no
independent `Sync` implementation is needed.

### `Send` and `Sync` for `arena::RunClaimTarget`

The target contains a raw address but never owns arena storage. It is created
only from a fully initialized published run and remains private to callers
which retain the heap. Its only shared mutation is an atomic lease-word CAS;
after a successful claim, the corresponding ordinary allocation word has one
exclusive worker. Boxed target records retain stable addresses until heap
teardown, and an allocation-class handle keeps that heap alive while loading
or copying the current target.

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
  borrow. C2C.1b's payload access additionally validates class identity,
  geometry, slot bounds, size, and alignment before its raw initialization.

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
  non-aliasing, separate-arena and separate-heap ownership, mask arithmetic at
  the highest representable complete chunk range, one indexed lookup
  independent of chunk count/order, and exact boundary-slot ownership across
  several chunks.
- Run-topology tests cover every empty header, independent adjacent class
  identities and geometry, zeroed bitmap ranges, exact first and last slots,
  rejection of non-slot addresses, and non-publication after invalid or
  repeated initialization.
- C3A tests force class discovery behind synthetic exclusive admission,
  prepare/admit/activate state, rollback before activation, same-heap recursive
  entry during a pending exclusive transition, independent cross-heap counts,
  committed exclusion of fresh outer entry, and mutator-exit visibility. The
  coordinator Loom models cover visibility and pending-exclusive priority.
- The ordinary crate checks, exact unsafe inventory, focused Miri run, and
  repository-wide checks are required at completed checkpoints.
- Miri passes all implemented tests with leak checking enabled. C1's temporary
  `-Zmiri-ignore-leaks` exception was removed with the last payload
  `Box::leak`; the process-wide canonical metadata registry remains reachable
  static state.
- The C1 pointer/access/trace/mutation surface is reviewed and frozen for the
  initial non-moving collector. A moving collector may add a distinct edge-
  rewriting contract; this observational visitor does not promise relocation.

## Governing Invariants

The cross-plan invariants are maintained in
[`../../docs/plans/GarbageCollectionRoadmap_2026-08-19.md`](../../docs/plans/GarbageCollectionRoadmap_2026-08-19.md).
This ledger restates only the concrete representation facts needed to audit
unsafe code; it does not create competing semantic rules.
