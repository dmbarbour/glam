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
adds heap-local dense allocation classes and the original typed class handles;
C4D later makes those handles collector-private. C2B.3
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
Release/Acquire pair or managed-data mutex, not through a load of the separate lease
word. C3 adds the heap-local mutator-admission coordinator, moves allocation-
class discovery behind mutator authority, and orders TLS entry as prepare,
admit, then activate. Production request epochs now elect one collector, drain
mutators, establish exclusive access, and perform a synthetic finalizer-mutator
handoff. There is still no tracing, reclamation, managed-payload finalization,
or collector callback. C4A converts allocation words to atomic single-writer/
multi-reader publication, adds the one-word typed `Root<T>` and weak-heap
`RootCell`, and checks heap ownership, canonical representation, and the
allocation bit in every root construction. C4B publishes each new cell as one
weak registry entry before returning its public root, then adds stable
exclusive traversal and in-place pruning without a strong-root snapshot. C4C
wires that walk into every elected collection and forces the final-public-drop
ordering on both sides of each temporary weak upgrade. The walk still performs
no marking or reclamation. C4D replaces the reusable public class handle with
a mutator-scoped `Allocator<'_, T>`, moves durable class/run topology entirely
under heap ownership, and removes allocation capabilities as heap owners.
C5A.0 splits mutator/collector coordination from managed heap data and makes
the coalesced collection request a sibling atomic. It changes no pointer,
allocation, root, tracing, or reclamation semantics. C5A.1 clears every
assigned run's contiguous ordinary mark-word range under exclusive authority
and adds checked per-slot test/set operations. C5A.2 recovers exact owner,
allocation state, and canonical metadata from every collector address before
marking. C5A.3 scopes worklists and scalar counters to one attempt, recovers a
poisoned managed-data mutex after an attempt panic, leaves partial marks as
unpublished scratch, and relies on the mandatory next-attempt clear. Full
graph tracing remains disabled through C5A. C5B reserves root work outside the
data lock, counts live root entries independently, marks each allocation before
enqueueing it, and traces the resulting graph through a checked non-recursive
worklist. Reclamation remains disabled.
C5C makes edge-driven worklist growth explicitly fallible, proves recovery
from trace and work-publication panics, and rejects live foreign, stale,
non-slot, and unallocated reported edges before unsafe dispatch. A failed
attempt still publishes no reachability result and reclamation remains
disabled.
C5D.1 consumes a drained successful mark attempt into scalar root-entry,
trace, distinct-mark, and conservative-retention counts. The latest report and
completed epoch publish together under the coordinator mutex; a failed attempt
changes neither. The bitmap remains heap-private and reclamation remains
disabled. C5D.2 closes the mark-only phase with an independent randomized
reachability oracle, repeated complete-run bitmap histories, and native
million-edge depth and width fixtures. It adds no unsafe operation, unsafe
module opt-in, or reclamation behavior. C6A.0 replaces the parameterless
exclusive-work hook with a data-side post-mark operation. It receives the
completed scalar summary and a temporary managed-data borrow under the same
exclusive collection authority, then releases that borrow before finalizer
admission. It still performs no classification or reclamation.

The crate denies unsafe code by default. `src/lib.rs` gives the reviewed
`pointer`, `root`, `mutator`, `trace`, `mutation`, `thread_cache`, and unit-test
modules named lint expectations for unsafe code. The exact module expectations
and every unsafe function, implementation, and block are checked into
`scripts/unsafe-modules.txt` and `scripts/unsafe-sites.txt`;
`scripts/audit-unsafe.sh` fails when either inventory changes.

## Arena Ownership Invariants

- Every arena chunk is one 8 MiB allocation with size and alignment equal to
  8 MiB. It therefore contains exactly 128 aligned 64 KiB runs.
- `HeapInner` owns its arena behind the managed-data mutex. The stable owning
  vector and an authoritative map from aligned chunk base to vector index are
  updated together. Both collections reserve capacity before publication;
  dropping a rejected candidate or its arena returns the allocation exactly
  once.
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

## Mark Attempt Invariants

- Mark words are ordinary `u64` values used only while the coordinator holds
  exclusive collection authority. Mutator allocation reads and writes only
  atomic allocation/lease state and payload slots; it never observes or
  changes a mark word.
- Every collection attempt clears each assigned run's complete contiguous mark
  range after all mutators drain and before root visitation or synthetic mark
  work. The mutable `u64` slice exists only for that exclusive bulk fill.
- Collector lookup starts from the erased integer address, finds its owning
  live chunk and exact slot-start through the indexed arena, validates header
  class and geometry against the dense class entry and run pool, loads the
  allocation bit with Acquire, and recovers the class's canonical static
  metadata. Foreign, interior, unallocated, absent-class, and unpublished-run
  addresses produce no `CollectorSlot`.
- A checked `CollectorSlot` is attempt-local and remains under the same
  managed-data guard. Its slot mark is the sole per-allocation reachability
  record; the attempt owns only a pointer worklist and scalar counters, not a
  run-keyed map or summary.
- Root seeding observes the stable registry length under exclusive authority,
  reserves that much additional worklist capacity outside managed data, then
  retains weak entries under the data lock. Every successful upgrade
  increments `root_count`, but only the unmarked-to-marked transition enqueues
  its pointer. Temporary strong cells are released during the shared retain
  walk, and no trace dispatch or fallible vector growth occurs inside root
  seeding.
- The worklist therefore contains only already-marked, unique `TraceWork`
  discoveries. Each item retains the erased pointer and canonical metadata
  recovered by checked discovery. Exclusive authority keeps that association
  stable until pop, and drain holds managed data during dispatch, so it does
  not repeat topology lookup: it dispatches the retained representation
  exactly once. Each reported edge repeats checked discovery and marks before
  enqueueing, which terminates cycles, diamonds, repeated edges, and duplicate
  roots without recursive Rust calls.
- C5D.2 compares those authoritative bits with an independent reachability
  computation across deterministic randomized graphs and checks zero, one,
  all, then zero marked allocations in one complete run across successive
  collections. Million-node depth and million-edge width fixtures establish
  that the same checked worklist remains non-recursive at scale; their observed
  worklist capacity is diagnostic evidence rather than a safety threshold.
- Edge-driven worklist growth calls `try_reserve` before `push`; allocation
  failure therefore unwinds through the same attempt guard as a `Trace` panic.
  There is an intentional failure window after a newly discovered edge is
  marked and before its work item is published. A panic in that window leaves
  only an unpublished scratch bit; dropping the attempt discards every queued
  proof, and the next attempt clears the bit before rediscovery.
- Checked discovery is the only constructor of production `TraceWork`. A
  visitor-reported pointer must identify an exact live slot in the collecting
  heap before its owning run's canonical metadata is retained. Foreign, stale,
  interior, and unallocated addresses therefore panic before unsafe trace
  dispatch. Pointer-only `ErasedGc` carries no independent claimed type;
  canonical representation mismatch remains a typed-root construction check,
  not a second worklist-drain check.
- If an attempt panics, Rust drops its worklist and counters before the
  collection guard recovers and clears any managed-data mutex poison. Recovery
  does not scan or clear partial marks. It relatches collection before
  restoring ordinary coordinator state, and the original panic resumes. The
  next attempt's mandatory initial clear makes every stale mark irrelevant.
- Only `MarkAttempt::finish` converts scratch into a successful `MarkSummary`,
  and it requires an empty worklist. The complete scalar summary remains local
  while later exclusive/finalizer work runs. `CollectionAttempt::complete`
  then stores the report and matching completed epoch in one coordinator
  critical section. A waiter sees neither or both. The coordinator retains
  only the latest report; an overtaken synchronous caller receives that later
  report when its epoch satisfies the caller's target.
- Successful marks require no copied bitmap, validity flag, identity list, or
  per-run summary. They are authoritative only as the completed attempt's
  heap-private reachability result: C6 will consume them under the same
  collection authority, and a later attempt clears them before reuse. The C5
  conservative-retention count is zero; C6 quarantine introduces the first
  slots retained without trace dispatch.

## Managed Pointer and Access Invariants

- `Gc<T>` is transparent over exactly one `NonNull<T>`. An unconditional const
  assertion latches its one-pointer width. It carries no heap, domain, class,
  allocation-record, or debug field and is not a root. It supports pointer-
  identity equality but deliberately does not implement `Hash`, because an
  address hash could not remain stable across later moving collection.
- `Mutator<'heap>` contains a reference to exactly one `HeapInner`. Its
  thread-cache handle contains thread-local `Rc<RefCell<_>>` state, making the
  token neither `Send` nor `Sync`; the lifetime prevents it from outliving that
  heap entry.
- `Mutator::allocator` returns `Allocator<'mutator, T>` with real borrows of
  that mutator's heap and thread cache. The higher-ranked `with_mutator`
  boundary prevents the allocator from escaping its admitted region or being
  sent to another thread. `Allocator::alloc` accepts only `T: Trace`; its inline
  const assertion rejects zero-sized types while compiling an invalid
  monomorphization, before any allocation can run. A public foreign
  allocator/heap combination is unrepresentable.
- Before reclamation exists, arena payloads never move and remain live until
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
period. `Allocator::alloc` obtains the pointer only after its synchronized arena
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

Correct pre-sweep calls satisfy the proof because C3's synthetic collection
reclaims nothing and arena payloads stay live until heap teardown. Wrong-heap and
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

- One coordinator mutex protects the admission phase, active-outer-mutator
  count, active/completed collection epochs, and every condition-variable
  predicate. Its sibling condition variable supplies wakeups only; every
  waiter loops and rechecks its complete predicate under that mutex.
- A separate managed-data mutex protects arena, class/run topology, allocation
  pressure, external-root registrations, and future mark/sweep state. No
  production path holds the coordinator and managed-data mutexes together. A
  collector validates coordinator authority, releases that guard, and then
  accesses managed data; `Exclusive` authority keeps the state stable where
  that transition needs stability. C6A.0 post-mark work runs under this data
  mutex and is therefore data-side only: it may inspect the already completed
  mark summary and authoritative bitmap, but must not acquire the sibling
  coordinator mutex. Its borrow ends before finalizer admission.
- The coalesced collection request is a sibling `AtomicBool`, not duplicated in
  either locked component. An asynchronous request is exactly one Release
  store: it acquires no mutex and sends no condition-variable notification.
  Admission and synchronous collection load it with Acquire ordering before
  attempting election under the coordinator mutex.
- `Ordinary` admits outer mutators. A nonblocking collection request is only a
  coalesced hint and denies no admission. An outer entry which observes
  `Ordinary`, zero active mutators, and a request atomically publishes
  `Exclusive`; otherwise it enters normally and leaves the hint latched. One
  epoch identifies the active collection, and synchronous waiters join it
  rather than requiring a follow-up epoch.
- Preparing a thread-local heap entry obtains or validates its weak heap-
  qualified record without changing recursive depth. An outer preparation then
  obtains one coordinator obligation before activating its cache. A panic or
  block between preparation and activation therefore leaves the TLS record
  inactive, and the admission token's destructor rolls back the active count.
- Recursive same-heap entry observes nonzero thread-local depth and reuses the
  outer coordinator obligation. It remains available while exclusive work is
  requested so an already-admitted mutator can finish its bounded region.
  Entry into a different heap uses that heap's independent TLS record and
  admission count. No dependent category is needed: an uncommitted request
  never blocks cross-heap entry, while an authoritative `Exclusive` phase
  blocks every outer entrant.
- Entry destruction first decrements recursive depth and makes the outer cache
  quiescent, then retires the coordinator obligation. Consequently, observing
  zero active mutators after acquiring the coordinator mutex also observes all
  work sequenced before those outer exits. C3's native forced schedules and
  Loom model latch this visibility edge.
- Outermost exit only makes its TLS cache inactive, retires its coordinator
  obligation, and wakes waiters when the active count reaches zero. It neither
  scans TLS records nor services collection. This makes nested cross-heap exit
  identical to every other outer exit.
- Before exclusive work, the collecting thread clears its complete inactive
  cursor cache for the target heap. The collector-to-finalizer handoff then
  changes `Exclusive` directly to `Finalizing` while installing one active
  mutator obligation under the same coordinator mutex. Ordinary finalization
  work and recursive same-heap entry therefore run with normal mutator
  authority. On
  successful completion an entry-elected collector carries that obligation
  directly into its originally requested outer entry; `collect_full` drops it.
  Successful completion resets pressure and clears the request while holding
  managed data, releases that guard, and then publishes ordinary coordinator
  state. Pressure cannot race that clear because publication uses the same
  managed-data mutex. Atomic modification order makes an external request
  before the clear coalesce into the active collection and one after the clear
  remain pending. Finalizing mutators may also publish roots or pressure in the
  interval before coordinator completion; those updates remain authoritative.
- An unwind guard covers exclusive and finalizer test work. It retires any
  installed finalizer mutator first, clears collector ownership, restores
  ordinary admission, and relatches the interrupted collection. C6 adds the
  stronger payload/destructor recovery rules when collection gains effects.
- An admitted mutator borrows the `Heap` handle's existing `Arc<HeapInner>`;
  admission does not clone a shared owner. Its scoped allocator borrows the
  mutator's heap and cache and carries only a collector-private, non-owning
  class identity. Neither admission nor allocation-class discovery creates a
  new heap owner.

## Heap-Local Allocation-Class Invariants

- Canonical metadata discovery and pure fixed-run geometry derivation finish
  before the managed-data mutex is acquired. Unsupported layouts therefore
  publish no heap-local state or dense ID.
- One managed-data mutex guards the arena, metadata-identity index, and dense
  class table. Metadata keys compare and hash only the canonical static
  descriptor address; `TypeId` is absent from this state.
- Immutable class candidates are constructed outside the managed-data lock. After the
  second winner check, vector and map capacity is reserved before either
  authoritative structure is changed, then the dense entry and its index are
  published under the same mutex.
- Heap state owns each durable class entry, run pool, and stable frontier cell.
  Collector-private `AllocationClass<T>` values carry a non-owning heap pointer,
  exact metadata address, dense class ID, and a clone of that stable frontier
  cell. They retain neither the heap nor any managed allocation.
- A public `Allocator<'mutator, T>` is constructible only through an admitted
  `Mutator` after metadata and geometry agreement. Its real heap/cache borrows
  make reuse after that region impossible in safe Rust. Repeated scoped
  discovery selects the same heap-owned class and frontier state; no
  cross-region cache is added yet.

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
  `RunLocation` is infallible and occurs under the same managed-data mutex. No
  observer can see a header without its class-pool membership.
- Checked slot resolution validates arena membership, exact slot-start
  geometry, dense class-table membership, class geometry, and authoritative
  run-pool membership before returning canonical metadata. A header ID from
  another heap has no meaning outside its owner state.
- Private `AllocationClass<T>` provenance is checked before the managed-data mutex and
  before any run state is changed. A foreign internal identity therefore cannot
  publish a run even when its numeric dense ID happens to equal a local ID.
  Ordinary `Allocator::alloc` relies on constructive provenance and retains a
  debug assertion rather than paying a release-build domain check per object.

## Synchronized Payload Allocation and Terminal Destruction

- The correctness allocator holds the managed-data mutex while selecting a free
  allocation bit, initializing its typed payload, and publishing that bit. The
  payload and bitmap word are disjoint validated run ranges, and no operation
  capable of unwinding occurs between their raw writes.
- Selecting a slot changes no persistent state. An injected unwind after
  selection but before initialization leaves the allocation bit clear and the
  input value is destroyed normally. The already published empty typed run may
  remain available for later allocation.
- Allocation-bit reads and writes use initialized aligned `u64` side metadata
  under managed-data exclusion. The synchronized path may scan run pools and
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
  relies on the managed-data mutex. The separate lease-word Acquire does not publish
  the run record; in C2C it participates in atomic word ownership. C5 will add
  a Release reset on that same lease word to publish rebuilt post-collection
  allocation/free state to the next Acquire claimant.
- The cursor carries the stable owning `RunAddress`, validated `RunGeometry`,
  one allocation-word index, and a local free mask. Its mask is the inverse of
  the authoritative allocation word intersected with the exact slot-count
  mask, so tail padding can never become a payload address.
- While a lease is live, only its worker-local cursor writes that allocation
  word. Other claims read it only after winning its lease bit; checked root
  construction and later collection may read any allocation word concurrently.
  Both allocation and lease words are `AtomicU64`. Distinct words occupy
  disjoint storage, so concurrent allocation within one run is data-race-free,
  while atomic reads make allocation state safely observable outside the
  writer. Size and alignment suitability are compile-time assertions; raw
  allocation and lease storage is initialized directly as `AtomicU64`, not
  reinterpreted from a live `u64`.
- The hot path performs every bounds, size, alignment, and free-bit assertion
  before initializing the payload. Its two final operations are an infallible
  payload write followed by a Release allocation-bit store; no unwind can
  expose uninitialized storage as allocated. Root validation uses an Acquire
  load before treating the payload as live. The writer updates its local free
  mask only after both writes.
- Allocation visibility is not delayed until mutator exit. Returning `Gc<T>`
  occurs only after bitmap publication, and ordinary Rust synchronization may
  immediately share it with another mutator. Collection remains disabled in
  C2C, so the payload then stays live through terminal heap teardown.
- Allocation pressure is charged exactly once under managed data after a typed
  run becomes authoritative in both arena and class pool. Word claims, local
  object allocation, cursor eviction, explicit cache release, and thread exit
  do not touch it. A saturating count latches the initial request after 7/8 of
  one chunk, currently 112 typed-run publications. C3 services that coordinator
  request but preserves the pressure latch and count; C6 owns rearming the
  allowance after successful sweep.
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

### `mutator::Allocator::alloc`

The safe allocator first attempts the worker-local cursor and then the class's
atomic run frontier. Only frontier exhaustion enters the synchronized topology
slow path. Every successful branch calls `Gc::from_raw` only after the exact
payload has been initialized and its allocation bit published. The allocator's
heap/cache borrows and private class identity are created together by one
mutator; a debug assertion checks that constructive invariant. Neither payload
path contains a panicking operation between the payload write and bit
publication.

### `thread_cache::AllocationCursor` raw run and bitmap access

The cursor's raw pointer arithmetic, payload initialization, and bitmap access
are justified by the atomic lease operation over a published stable run record:
it carries validated typed-run geometry, masks invalid tail bits, and grants
this thread exclusive ownership of one represented allocation word. The cursor
checks the selected slot, payload size, and alignment before writing. No other
allocator may mutate its allocation word until a future full collection revokes
all leases after stopping mutators and advancing the heap epoch. Checked root
construction may inspect it through an Acquire atomic load without taking the
lease or interfering with the sole writer.

### `arena::ArenaChunk` allocation and destruction

`alloc_zeroed` receives one checked nonzero `Layout` whose size and alignment
are both the fixed arena-chunk size. A null result becomes `AllocationFailed`
before any chunk is published. The successful pointer remains uniquely owned
by its `ArenaChunk`, whose `Drop` returns it through `dealloc` with the identical
layout.

The `NonNull::add` sites derive aligned run starts, side-metadata words, or
bounded payload slots. Each is preceded by checked run membership and validated
geometry, proving the result remains within the live chunk. Payload and
mark-bitmap writes occur only under exclusive arena access. Allocation and
lease words are initialized and subsequently accessed as `AtomicU64`; one
leased worker owns allocation-word writes, while root validation and later
collector work may read them. Read-only recovery does not create a payload
reference.

The mark range is initialized as ordinary `u64` storage and remains disjoint
from the header, atomic allocation/lease words, alignment padding, and payload.
After exclusive admission drains mutators, the collector reconstructs one
bounded mutable slice over the contiguous words and uses `fill(0)` for the
mandatory initial clear. Checked slot index and bitmap geometry then derive one
word pointer for test/set. The collector is the sole reader and writer until it
leaves exclusive work, so these operations require neither atomics nor
fine-grained locks. Every raw read, write, and slice construction stays within
the live run selected from the owning arena rather than deriving from an
unvalidated address.

### `Send for arena::ArenaChunk`

An arena chunk owns raw untyped bytes and exposes no Rust reference. Moving the
owner between threads transfers the one deallocation obligation without
accessing those bytes. Sharing remains mediated by the heap's arena mutex; no
independent `Sync` implementation is needed.

### `Send` and `Sync` for `arena::RunClaimTarget`

The target contains a raw address but never owns arena storage. It is created
only from a fully initialized published run and remains private to callers
which retain the heap. Its only shared mutation is an atomic lease-word CAS;
after a successful claim, the corresponding atomic allocation word has one
exclusive writer but may have concurrent atomic readers. Boxed target records
retain stable addresses until heap teardown. Only a scoped allocator may load
or copy the current target; its admitted mutator borrow prevents heap teardown
until that access ends, without making the allocator another heap owner.

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
type metadata. C5B's typed-run lookup recovers and validates those facts before
the edge can be marked or dereferenced. The receiver may panic; no visitor or
traversal state is stored in the traced object.

`ErasedGc` is `Send + Sync` because it grants no pointer access and is created
only from `Gc<T>` where `T: Trace` already requires `Send + Sync`. Sending or
sharing the address does not bypass the later exact heap, allocation, and
metadata checks required before dereference.

### `root::Root<T>` construction and access

`Mutator::root` resolves the candidate address through the heap's indexed
chunk, run, class, and slot topology under managed data, compares the run's
canonical metadata with `metadata_for::<T>()`, and loads the atomic allocation
bit with Acquire ordering. It publishes one `Weak<RootCell>` into the same
locked managed data before returning the first public root. Rejected input is
reported only after releasing the mutex, so a caller contract violation does
not poison an otherwise valid heap. Clones share the existing cell and neither
clone nor drop touches the registry.

The `Arc<RootCell>` and its weak registration candidate are constructed before
the managed-data mutex is acquired. Holding one critical section across validation and
the vector push is a one-lock implementation choice, not the semantic barrier:
the calling `Mutator` already prevents an exclusive collection or reclamation
from intervening. Current validation nevertheless needs the mutex to read the
arena chunk index and class topology; only its final allocation-bit load is
independently atomic.

`Root<T>` contains one `Arc<RootCell>` plus a zero-sized typed marker. The
non-generic cell contains a `Weak<HeapInner>` and one `ErasedGc`. The weak
control block remains allocated while the root exists, so comparing its address
with the live mutator's `Arc` cannot mistake a reused heap allocation for the
original heap. The cell never upgrades that weak reference merely to read a
value, and a root does not retain or re-enter its value domain.

`Root::get` rejects a nonmatching mutator in every build before reconstructing
the private typed `Gc<T>` and invoking its existing unsafe access gateway. The
private constructor established the representation, allocation, and heap
invariants; the live matching mutator prevents reclamation for the returned
reference's lifetime. C4 performs no reclamation. Dropping `RootCell` releases
only its weak heap reference and pointer bits; it never dereferences or destroys
the managed payload and invokes no user code.

Exclusive root traversal holds the managed-data mutex after the
coordinator has stopped every mutator. No new root cell can therefore be
validated or published during the walk. `Vec::retain` upgrades each weak entry
for exactly one visit, drops that temporary strong reference before advancing,
and compacts failed upgrades in place while preserving registration order. A
successful upgrade conservatively keeps its seed live for that collection even
if the final public root is dropped concurrently; a failed upgrade cannot later
become live because no strong cell reference remains.

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
- C3 tests force class discovery behind exclusive admission,
  prepare/admit/activate state, rollback before activation, same-heap recursive
  entry with a request latched, independent cross-heap counts, authoritative
  exclusion of fresh outer entry, mutator-exit visibility, idle-entry election,
  direct admission handoff, collector-cache reset, request coalescing,
  any-mutator synchronous rejection, reciprocal nested entry, already-exclusive
  targets, no exit-time service, the no-gap finalizer handoff, pressure
  acknowledgement, and panic restoration. Coordinator Loom models cover
  visibility, unique idle-entry election, reciprocal requested-heap admission,
  and exclusive-to-finalizer-to-entry authority transfer.
- C4A tests the root handle's one-word and `Send + Sync` contracts, exact
  allocated-slot and representation validation, all-build foreign-heap
  rejection during construction and access, later-region and cross-thread
  access with a live heap, concurrent allocation-word observation, and heap
  teardown while a root cell remains escaped.
- C4B tests one registry entry per cell rather than per clone, publication only
  after successful validation and mutator admission, stable ordered visitation,
  and in-place removal of dead weak entries through the exclusive walk.
- C4C tests production collection-path pruning, a final public root dropped
  after the collector's successful per-entry upgrade, release of that temporary
  strong reference before visiting the next entry, conservative retention until
  the following walk, passive root-cell destruction under the managed-data mutex, and
  exclusion of replacement publication until the collector leaves exclusive
  authority.
- C5A.0 tests that asynchronous request takes neither component lock and sends
  no coordinator notification; pressure and explicit requests on the two sides
  of data-side acknowledgement have the specified coalescing behavior; a
  finalizing mutator may publish a root and new pressure in the inter-lock
  interval; and a synchronous requester joins an active epoch while its
  collector is blocked on managed data. Existing admission, finalization,
  root-publication, panic, and Loom schedules pass unchanged after the split.
- C5A.1–3 tests bulk clear across zero, one, and three assigned runs; first and
  duplicate marks on both sides of a bitmap-word boundary; unchanged mark
  state across mutator allocation; exact owner and canonical-metadata recovery;
  rejection of foreign, interior, unallocated, absent-class, and unpublished-
  run addresses; and injected panics after zero, one, and three distinct-run
  marks. The panic fixture observes physical scratch marks before retry, proves
  allocation/root state and the original panic payload survive, verifies mutex
  poison recovery and ordinary coordinator restoration, and then proves a
  clean retry starts with empty scratch state and cleared bitmaps.
- C5B tests separate root-entry count from unique discovered allocations,
  verify clone-versus-distinct-root registry behavior and dead weak-entry
  pruning, and prove that trace dispatch begins only after root seeding. Exact
  non-recursive traversal covers duplicate edges, a cycle, a diamond, an
  unrooted allocation, a native 20,000-node chain, and 2,048 wide branches
  sharing a 64-node tail. Miri runs the same linear-chain path with 256 nodes;
  native execution retains the stack-depth stress while interpretation checks
  the same allocation, provenance, discovery, and worklist operations at
  bounded cost. Every reachable allocation traces once despite repeated paths.
- C5C injects trace and work-publication panics after zero, one, and many
  reported edges or completed pushes. The tests observe the exact partial mark
  scratch, original panic payload, mutex-poison recovery, restored admission,
  relatched request, and successful clean retry. Separate adversarial fixtures
  report a live foreign pointer, a pointer from a dropped heap, an interior
  address, and an unallocated exact slot. Each is rejected before target trace
  dispatch, repeats while its invalid holder remains rooted, and leaves the
  involved heaps usable after that root is released.
- C5D.1 verifies exact root-entry, trace, and distinct-mark counts with
  duplicate edges, distinct roots, root clones, and an unreachable object.
  Coalesced synchronous requesters receive the same report. A forced pause
  between data acknowledgement and coordinator completion observes neither a
  completed epoch nor a report, while an entry-elected collection publishes
  both. A failed trace after an earlier success preserves that exact earlier
  report until a clean retry publishes its successor.
- C6A.0 verifies that post-mark work receives exact root, trace, distinct-mark,
  and conservative-retention scalars together with the authoritative marked
  slot while collection remains exclusive. The following finalizer callback
  can immediately acquire managed data, proving that the post-mark guard and
  borrow did not cross admission. Existing post-mark panic, request
  coalescing, failed-mark retry, report, and forced-order fixtures pass through
  the same refactored pipeline.
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
