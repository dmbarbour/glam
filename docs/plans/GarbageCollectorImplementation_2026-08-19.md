# Glam GC Subcrate Implementation Plan — 2026-08-19

Status: in progress; Phases C0, C1, C2A, and C2B are complete. The mandatory
post-C1 review is complete, and C2C.1a is next.

This plan implements an exact, non-moving, runtime-local tracing collector
without depending on Glam value semantics. The governing requirements and
integration gates live in
[`GarbageCollectionRoadmap_2026-08-19.md`](GarbageCollectionRoadmap_2026-08-19.md).

The deliverable is a stop-the-world full collector. Moving and generational
collection, including a moving nursery, remembered sets, and promotion, belong
to a later performance plan. Concurrent marking is also a later plan.

## Phase Status

| Phase | Status | Outcome |
| --- | --- | --- |
| C0 | completed | crate, provenance, safety, and test scaffold |
| C1A | completed | prototype managed pointer and access boundary |
| C1B | completed | trace visitor and representative structural traces |
| C1C | completed | mutation gateway, unsafe audit, and API freeze |
| C2A.1 | completed | pure run/slot/bitmap geometry and provisional run size |
| C2A.2 | completed | aligned arena chunks and checked owner-address recovery |
| C2A.3 | completed | run headers, side bitmaps, and topology verification |
| C2B.1 | completed | canonical process-wide object metadata and erased dispatch |
| C2B.2 | completed | heap-local allocation-class discovery and typed handles |
| C2B.3 | completed | typed-run publication, enumeration, and metadata resolution |
| C2C.1a | pending | indexed chunk ownership and checked owner lookup |
| C2C.1b | pending | synchronized arena allocation and prototype access transition |
| C2C.2 | pending | heap-specific TLS identity, epochs, and cache lifecycle |
| C2C.3 | pending | bitmap-range leasing and worker-local hot allocation |
| C2C.4 | pending | pressure, panic, teardown, and allocator audit |
| C3A | pending | ordinary admission and same-heap recursion integration |
| C3B | pending | collector election and single-heap STW quiescence |
| C3C | pending | cross-heap dependent admission |
| C3D | pending | finalizer handoff, pressure, panic, and teardown races |
| C4 | pending | explicit external roots |
| C5 | pending | exact full marking |
| C6A | pending | no-drop sweep and run-state publication |
| C6B | pending | finalizer detachment, mutator execution, and non-resurrection |
| C6C | pending | destructor panic, quarantine, retry, and activity |
| C6D | pending | terminal heap teardown and final safety audit |
| C7 | pending | shared-pointer and worker-shaped stress |
| C8 | pending | tuning and final collector audit |

## Intended Crate Shape

```text
crates/
  glam-gc/
    Cargo.toml
    LICENSES/
    src/
      lib.rs                 # narrow safe facade
      heap.rs                # heap ownership and collection policy
      mutator.rs             # region entry, recursion, safepoints
      pointer.rs             # Gc and Root
      trace.rs               # unsafe trace contract and visitor
      class.rs               # TypeId discovery and erased object metadata
      arena.rs               # large heap-owned arena chunks
      run.rs                 # aligned typed runs, bitmaps, slot allocation
      roots.rs               # explicit root registry
      mark.rs                # full tracing
      sweep.rs               # reclamation and destruction
      metrics.rs             # operational counters and test inspection
```

The root manifest becomes a Cargo workspace containing `.` and the approved
path crates while retaining the existing Glam package, library, and binary.
This is implementation support, not a split of the Glam product. The collector
crate is not a supported embedding API during this transition.

## Initial API Sketch

Names remain provisional until Phase C1C:

```rust
pub struct Heap { /* shared heap ownership */ }
pub struct Mutator<'h> { /* !Send region authority */ }
pub struct Gc<T: Trace> { /* Copy, non-rooting */ }
pub struct Root<T: Trace> { /* Clone, Send + Sync as appropriate */ }
pub struct AllocationClass<T: Trace> { /* heap-local dense class identity */ }

impl Heap {
    pub fn with_mutator<R>(&self, f: impl for<'h> FnOnce(&Mutator<'h>) -> R) -> R;
    pub fn allocation_class<T: Trace>(&self) -> Result<AllocationClass<T>, UnsupportedLayout>;
    pub fn request_collection(&self);
    pub fn collect_full(&self) -> CollectionReport;
}

impl Mutator<'_> {
    pub fn alloc<T: Trace>(&self, class: &AllocationClass<T>, value: T) -> Gc<T>;
}

impl<T: Trace> Gc<T> {
    pub unsafe fn get_unchecked<'h>(&self, mutator: &'h Mutator<'h>) -> &'h T;
}

impl<T: Trace> Root<T> {
    pub fn get<'h>(&self, mutator: &'h Mutator<'h>) -> &'h T;
}

pub unsafe trait Trace: 'static {
    fn trace(&self, visitor: &mut Visitor<'_>);
}
```

The API must not promise `Deref`, Glam-visible finalizer declarations,
resurrection of a completed dead allocation, moving collection, weak pointers,
or arbitrary cross-heap conversion. It must still run ordinary Rust
destruction for managed payloads inside a collector-controlled mutator phase.

## Phase C0 — Provenance, License, and Verification Scaffold

- Create the path crate with `#![deny(unsafe_op_in_unsafe_fn)]` and warnings as
  errors under the repository checks.
- Add a root-workspace declaration without changing the existing `glam`
  package, library, binary, or default build behavior.
- Record the exact versions, revisions, files, and licenses of any code copied
  or adapted from Sandpit, Abfall, gc-arena, Rudo, or another implementation.
- Preserve upstream copyright and license notices beside derived code.
- Keep an explicit `SAFETY.md` listing unsafe modules, invariants, and audit
  status.
- Add test-only deterministic hooks behind a private feature; production code
  must not branch on them when the feature is disabled.
- Establish `cargo test -p glam-gc`, Miri, Loom, and sanitizer jobs or scripts.

Verification:

- the crate builds independently without importing Glam;
- license/source provenance is complete;
- `cargo geiger` or an equivalent report establishes the initial unsafe
  surface; and
- an empty heap can be created, entered, and dropped on one and several
  threads.

C0 completed on 2026-08-19 with these deliberately narrow decisions:

- the root package became the default member of a workspace which also
  contains `crates/glam-gc`, preserving the behavior of unqualified root Cargo
  commands;
- `glam-gc` has no normal dependency and contains no copied or adapted
  third-party source; Loom is an unmodified development dependency;
- the empty `Heap` is a cheaply cloned shared ownership shell, while its scoped
  `Mutator` is non-`Send` and non-`Sync` but offers no operation. C1A may still
  change token and provenance representation before managed pointers exist;
- `SAFETY.md` records an empty unsafe inventory, latched by compiling every
  crate target and feature with `unsafe_code` forbidden; and
- stable checks and the Loom API smoke model run through the crate-local
  verification script. Miri and sanitizer scripts are present but require an
  appropriately installed nightly toolchain.

## Phase C1 — Trace and Access Contract Spike

Implement only enough allocation leakage to test the internal pointer and
mutator safety shape.

Execute C1 as three independently verified checkpoints:

- **C1A — pointer and access boundary.** Introduce the prototype allocation
  record, pointer-only `Gc<T>`, heap-qualified `Mutator` access, compile-fail
  lifetime/thread tests, debug wrong-heap/type checks, and the first explicit
  unsafe-ledger entries. No trace traversal is required to pass C1A.
- **C1B — trace contract.** Add `Trace`, its edge visitor and erasure boundary,
  representative manual recursive traces, and only the structural helpers
  justified by those fixtures. Prove tracing is observational and retryable
  after panic; do not add marking or sweeping.
- **C1C — boundary audit and freeze.** Add the structural no-op mutation
  gateway, verify multi-heap token coexistence, run the focused Miri/unsafe
  inventory, and freeze the internal pointer/access/visitor surface before
  allocator work.

Each checkpoint runs its focused tests and the crate verification script. C1C
also performs the complete unsafe-ledger review for everything introduced by
C1A-C1C.

Roadmap Gate G0 was established on 2026-08-20 in
[`GarbageCollectionGateG0Baseline_2026-08-20.md`](GarbageCollectionGateG0Baseline_2026-08-20.md).
C0's crate and verification scaffold did not by itself satisfy that gate;
the baseline now latches semantics, memory, and representative timings before
unsafe pointer work. Replace C0's
temporary whole-crate `unsafe_code` prohibition check with an explicit expected
unsafe inventory which fails on unreviewed modules or sites, and update
`SAFETY.md` as each site is introduced.

- Represent `Gc<T>` as one typed non-null managed pointer with no carried heap,
  domain, class, or debug token. `T` prevents ordinary accidental type confusion;
  only the allocator and later reviewed tagged-value casts may construct it.
- Require a `Mutator` token for every dereference and allocation. Bare managed
  dereference is an internal unsafe primitive: the caller must prove that the
  pointer is live, belongs to the mutator's heap, and has representation `T`.
  The supported Glam embedding API never exposes that primitive.
- In debug/test builds, have the unsafe gateway verify every fact available
  from the prototype allocation record and, once typed runs exist, assert heap
  ownership, allocated slot alignment/state, and canonical object-metadata
  pointer equality. These assertions diagnose violations; release correctness
  must follow from construction, rooting, and mutator invariants rather than
  from a runtime check.
- Make `Mutator` non-`Send` and non-`Sync`; make `Gc<T>` `Copy`, `Send`, and
  `Sync` only under documented `T` bounds.
- Permit one thread to hold mutator tokens for several heaps concurrently.
  Token authority and every ambient lookup remain heap-qualified; there is no
  process- or thread-global singular "current heap" assumption.
- Define the unsafe `Trace` contract, including interior mutability, no hidden
  managed pointers, panic behavior, and destructor restrictions.
- Make tracing visitor-based. Implementations submit the managed edges selected
  by their actual representation; they do not publish fixed field-offset
  tables. Immediate values and non-edge list data are omitted by the
  implementation which understands them.
- Require tracing to be observational: it may report edges but may not mutate
  the managed graph or collector metadata except through its visitor. A panic
  from an otherwise conforming trace may abort that collection attempt without
  making the graph unsafe to trace again.
- Define a small managed-edge mutation gateway whose collector action is an
  inline no-op for full stop-the-world collection. This latches the API shape
  without imposing remembered-set or concurrent-marking work prematurely.
- Implement tracing manually for the C1B representative graph types. Provide
  narrowly reviewed structural implementations or visitor helpers for wrappers
  which contribute no representation policy of their own, initially options,
  fixed arrays, tuples, and slices as actual use requires. Do not add a derive
  crate or generic field-reflection abstraction in this plan: Glam has a
  bounded representation inventory, and persistent collections,
  synchronization cells, roots, external storage, and immediate fields require
  explicit edge policy.
- Specify equality, pointer identity, debugging, and heap-mismatch behavior.
- Keep raw `Gc<T>` construction crate-private and unsafe throughout this GC
  plan. Specify the obligations shared by allocator implementations and any
  future representation decoder: the address is non-null and properly aligned,
  identifies a live managed slot in the protected heap, and the slot's
  canonical metadata describes `T`. A later tagged-value phase may add one
  separately reviewed unsafe cross-crate integration gateway; because
  `glam-gc` is not a supported embedding API, that does not make raw
  construction a general client facility. `glam-gc` does not specify a tagged
  value, tag-to-type mapping, or serialized representation.
- Reserve an abstract run-owner lookup boundary independent of payload-slot
  alignment and without assigning Glam immediate tags. Managed payloads retain
  their Rust type alignment, including any `repr(align)` selected by their
  defining crate. Do not encode a run size or class in `Gc<T>`: the collector
  recovers the owner from the untagged address and its fixed run geometry.
  Variable run sizes and pointer-encoded run classes remain a profiled
  representation alternative, not a C1C commitment.

Verification:

- compile-fail tests show that references cannot escape a mutator region and a
  mutator cannot be sent to another thread;
- nested closures may hold distinct non-`Send` mutator tokens for two heaps at
  once without making either token authoritative for the other heap;
- shared `Gc` handles may move between threads; registered `Root` sharing is
  deferred to C4;
- there is no safe bare-`Gc` dereference API, and the unsafe operation's result
  lifetime remains bounded by its mutator token;
- forced wrong-heap and wrong-representation accesses trip debug assertions at
  the unsafe gateway without adding fields or release checks to `Gc<T>`;
- tracing a duplicate pointer is harmless; and
- manual traces visit an independently stated expected edge multiset for
  representative structs, recursive enums, and each admitted structural helper.

Checkpoint: freeze the internal API before building the allocator. Revisit the
integration plan if using the token in real evaluator call paths would require
an unacceptable semantic or visibility change.

C1A completed on 2026-08-21 with the following deliberately temporary shape:

- `Gc<T>` is exactly one typed `NonNull<T>`. It carries no heap identity,
  allocation record, class, or debug token; equality observes only managed-
  pointer identity, and there is no `Deref` or `Hash` implementation. Its
  transparent representation and unconditional const assertion latch that
  pointer-width contract. Omitting `Hash` avoids promising stable address
  hashes which a later moving collector would have to preserve or rebuild
  inside hashed containers.
- `Mutator::alloc` accepts `T: Trace`, fully initializes and
  leaks a `Box<T>`, registers its address in debug/test builds, and returns a
  non-rooting `Gc<T>`. An inline const assertion rejects zero-sized types while
  compiling an invalid monomorphization, demonstrated by an error-code-checked
  compile-fail doctest. C2 replaces this path rather than extending it into an
  allocator.
- A mutator retains a direct reference to its creating `HeapInner`, so all
  allocation and access checks are heap-qualified. One thread can nest regions
  from separate heaps without either token gaining authority over the other.
- The debug/test allocation record stores an address, `TypeId`, and type name.
  It verifies the facts available before typed runs exist, and is dropped with
  the heap even though prototype payloads remain intentionally leaked. This is
  not the canonical object-metadata design planned for C2B.
- Raw construction remains crate-private and unsafe. Raw access is an unsafe
  surface of the unsupported transition crate; its reference lifetime is tied
  to a mutator borrow, while compile-fail examples prove that neither the
  reference nor a mutator can escape its region in the prohibited ways.
- The crate denies unsafe code by default. Reviewed implementation and test
  modules opt in explicitly, while `scripts/audit-unsafe.sh` compares every
  unsafe construct and every module opt-in with checked-in exact inventories.
  [`SAFETY.md`](../../crates/glam-gc/SAFETY.md) records the proof obligations.

C1A adds no trace contract, roots, reclamation, safepoint coordination, or
production Glam integration. C1B may now add only the visitor-based trace
contract and representative structural traces described above.

C1B completed on 2026-08-21 with these boundaries:

- `Trace` is an unsafe `Send + Sync + 'static` contract. Implementations must
  synchronously report every represented managed edge, may report duplicates,
  must not mutate observable graph state, and must remain safely retraceable
  from the beginning after either implementation or visitor panic.
- `Visitor::visit` erases only the Rust pointer type. The erased pointer gains
  no heap, class, allocation, or trace metadata; typed runs remain responsible
  for recovering those facts later. Visitor construction stays crate-private
  until marking consumes the boundary.
- `Gc<T>` and prototype allocation now require `T: Trace`. The admitted
  structural implementations are only `Gc<T>`, `Option<T>`, fixed arrays, and
  pairs, plus the immediate `()`, `u32`, and `u64` types required by C1A's
  fixtures. Slices, general containers, derive support, and representation-
  specific persistent structures remain deliberately absent.
- Representative manual struct and recursive-enum implementations have exact
  independently stated edge-sequence tests. Those tests include repeated
  pointers and all admitted structural helpers.
- A visitor which panics partway through a trace leaves the same object able to
  reproduce its complete original edge sequence on a fresh attempt. Tracing
  creates no retained traversal state in either the object or collector.

C1B adds no root discovery, mark state, worklist, collection, sweep, or
production ownership migration. C1C may now add the no-op structural mutation
gateway and perform the planned boundary audit before allocator work.

C1C completed on 2026-08-21:

- `Mutator::with_edge_replacement` is the raw managed-edge mutation gateway.
  It reports the transition to the collector and then invokes the caller's
  closure; the gateway does not itself rewrite the represented storage. Its
  caller identifies the owner plus old and new edge, proves all three belong
  to the mutator heap, and performs the exact replacement inside one closure.
  Debug builds reject foreign pointers before invoking the closure. The
  current always-inlined collector hook is empty; future concurrent or
  generational plans may conservatively process both edges before mutation.
- The raw gateway remains unsafe because pointer-only `Gc<T>` cannot validate
  heap provenance at zero release cost. Production integration must provide
  representation-local safe wrappers which own that proof rather than expose
  the raw operation as an ordinary value API.
- `Gc<T>` remains pointer-sized, non-rooting, non-`Deref`, and deliberately
  non-`Hash`. Pointer equality and debug addresses are operational identities,
  not stable serialized data. The supported access path remains a scoped
  mutator plus a runtime-owned wrapper which discharges the raw access proof.
- `Trace` and its erased visitor are frozen for the initial non-moving
  collector. They enumerate edges observationally and expose a private erased
  address for typed-run lookup. Moving collection remains free to add a
  separate rewriting/relocation contract; C1 does not claim that observational
  tracing alone can rewrite pointer slots.
- Nested regions from two heaps prove that mutator authority remains heap-
  qualified. The exact unsafe-site and module inventories pass, and Miri
  passes all 14 unit tests with only the documented C1 prototype-leak check
  disabled. C2 must remove that exception when it removes `Box::leak`.

### Mandatory Post-C1 Review

The 2026-08-21 review found the later non-moving collector phases semantically
aligned with the C1 boundary, with these precision and partition updates:

- Split C2A into C2A.1 pure geometry, C2A.2 aligned arena ownership, and C2A.3
  concrete run-header/bitmap topology. Each checkpoint has an independent
  unsafe surface and verification story; payload allocation still waits for
  C2C.
- C2B owns monomorphized erased trace/drop dispatch and replacement of C1's
  debug `TypeId` allocation records with canonical metadata pointers. It does
  not add relocation dispatch while moving collection is deferred.
- C2C replaces the leaking prototype allocator, changes allocation to require
  a heap-local class, and removes Miri's leak exception. Its existing
  `ThreadHeapState` checkpoint remains the correct precursor to C3.
- C3A-C3D remain appropriately separated by ordinary admission, single-heap
  exclusion, cross-heap dependent admission, and finalizer handoff. C1's
  nested-heap test is only a token-shape regression, not a substitute for
  those state-machine tests.
- C4-C6 continue to own release-validating safe roots, exact marking, sweep,
  and finalization. The observational C1 visitor is sufficient for this
  non-moving plan; moving pointer-slot rewriting remains deferred explicitly.
- Integration I4 and later mutable representations must wrap C1C's unsafe raw
  gateway at the representation boundary and keep exact tracing synchronized
  with every managed-edge change.

No additional design question blocks C2A.1.

## Phase C2A — Arena Chunks, Fixed Typed-Run Geometry, and Layout Limits

Execute C2A as three independently verified checkpoints:

- **C2A.1 — pure geometry.** Define checked header/side-metadata/slot geometry,
  reject unsupported layouts, reconcile I0's measurements, and select one
  provisional fixed `RUN_SIZE`. Allocate no arena memory.
- **C2A.2 — arena ownership.** Reserve and release aligned arena chunks, divide
  them into fixed runs, and implement numeric range validation plus checked
  run-owner address recovery. Do not initialize allocation classes or payload
  slots.
- **C2A.3 — run topology.** Initialize run headers and allocation, lease, and
  mark side bitmaps from C2A.1 geometry; verify adjacent run/chunk topology and
  expose only the private checked owner/class boundary needed by C2B. Payload
  allocation remains disabled.

Run focused tests, the exact unsafe inventory, and Miri after each checkpoint.
Do not begin C2B until all three leave arena destruction and failed
initialization paths fully owned.

- Reserve large heap-owned arena chunks whose address and size are multiples of
  one fixed, power-of-two `RUN_SIZE`. Divide each chunk into runs of exactly
  that size. Every run contains one homogeneous allocation class and slot
  layout.
- Select one provisional `RUN_SIZE` at this checkpoint from measured
  header/bitmap overhead, cold-class fragmentation, and representative object
  layouts. Comparing other fixed sizes is tuning; supporting several sizes is
  deferred.
- Let the pure geometry input contain the Rust payload `Layout` and an optional
  requested total slot extent before alignment rounding. It is not additional
  padding. `None` means the Rust payload size; `Some(bytes)` must be at least
  that size, and canonical metadata discovery enforces this through generic
  const evaluation. The pure geometry helper retains a defensive error for
  arbitrary internal inputs. Round the requested extent up to
  `Layout::align()` to obtain the actual stride, which may therefore be larger.
  Owner lookup from an untagged pointer depends only on fixed run alignment,
  not on payload alignment or stride. The collector assigns no semantic
  meaning or tag budget to either.
- Given an untagged managed address, recover the run header by masking with the
  fixed `RUN_SIZE`. In debug/test configurations, first validate the numeric
  address against the mutator heap's arena-chunk ranges without dereferencing a
  candidate header, then validate the recovered run and allocation class. Raw
  construction still forbids arbitrary or stale addresses in every build.
- Reject zero-sized payload layouts. Glam has no managed zero-sized
  representation, and allocation identity must always correspond to a real
  slot address.
- Implement a pure checked geometry calculation from Rust `Layout` plus the
  metadata-requested slot size to slot stride, first-slot offset, slot count,
  and bitmap geometry. It accounts for its own side metadata and either yields
  at least one valid slot or rejects the request. C2B applies this calculation
  to `ObjectMetadata` when it creates the heap-local allocation class.
- Consume I0's preliminary production layout inventory when selecting the
  provisional run size. If I0 is not yet complete, record equivalent
  representative layout measurements here and reconcile them with I0 before
  any production ownership migration.
- Store allocation, allocation-range leases, and mark state in run-side
  bitmaps. The lease bitmap has one bit per allocation-bitmap word and is part
  of the checked geometry. Do not reserve card metadata or generation fields
  for the initial collector.
- Set a documented maximum managed size and alignment derived from the fixed
  run geometry. Reject unsupported layouts in the geometry calculation; C2B
  propagates that result from allocation-class creation. Do not implement a
  large-object fallback, multi-run object, heterogeneous run, or arbitrary DST
  path.
- Keep variable-sized byte buffers, arbitrary host payloads, and other values
  which do not fit the limit in audited external storage or decompose them in a
  later Glam representation project.
- Keep the header-address formula private. Expose only checked owner/class
  lookup needed by pointer access and the future tagged-value layer.

Verification:

- every valid address in every arena chunk maps to exactly one fixed-size run
  header, including the first and last slots of adjacent runs;
- adjacent runs and arena chunks never alias;
- derived slot indices match the stride and reject header,
  padding, interior, and non-slot addresses;
- representative layouts use the same run byte size while deriving independent
  slot strides and counts;
- Rust layouts and requested slot sizes producing 16-, 24-, and 32-byte
  strides yield correct slot and bitmap geometry;
- smaller slots measure and report their larger bitmap overhead rather than
  being rejected merely to preserve a value-layer tag budget;
- unsupported size/alignment layouts fail geometry derivation without
  partially allocating storage; dynamically sized types have no class-entry
  path because the planned `AllocationClass<T: Trace>` keeps its implicit
  `Sized` bound;
- pointer ownership checks distinguish heaps in debug/test builds; and
- Miri and property tests cover mask arithmetic, alignment, and the maximum
  supported address range.

### C2A.1 Completion

C2A.1 completed on 2026-08-21:

- The provisional fixed run size is 64 KiB. It bounds cold-class waste to one
  modest run while giving the I0 representative 16-, 24-, 32-, 144-, 192-,
  and 256-byte layouts respectively 4,028, 2,699, 2,029, 453, 340, and 255
  slots. Allocation, lease, and mark side metadata consumes about 1.55%,
  1.06%, 0.79%, 0.21%, 0.16%, and 0.11% of those runs. A deliberately tiny
  one-byte class remains supported with about 20.1% bitmap overhead rather
  than defining value-layer alignment policy in the collector.
- Pure checked geometry derives allocation, allocation-word lease, and mark
  bitmap ranges, aligned first-slot offset, stride, count, and exact slot
  indices. Binary search selects the greatest fitting slot count without
  allocating or mutating heap state.
- The geometry exposes a minimally aligned maximum managed size of 65,448
  bytes and maximum supported power-of-two alignment of 32 KiB. Larger or
  overflowing layouts, zero-sized payloads, and undersized slot requests fail
  derivation without state.
- Focused boundary and sampled-layout tests, the exact unsafe audit, the full
  collector check, and Miri all pass. C2A.1 adds no unsafe code or arena
  allocation.

### C2A.2 Completion

C2A.2 completed on 2026-08-21:

- Each heap now owns an arena capable of reserving zeroed 8 MiB chunks aligned
  to their full size. Each chunk contains exactly 128 logical 64 KiB runs and
  returns its storage exactly once through RAII; no payload or run-header type
  is initialized yet.
- Numeric owner recovery validates a candidate address against live chunk
  ranges before masking it to a run boundary and deriving a pointer from the
  original allocation. Addresses at both ends of every run, the final chunk
  byte, and the highest representable complete chunk range are covered.
- Chunk publication rejects an allocation failure or any overlap before
  changing arena state. Separate live chunks, arenas, and heap-owned arenas
  cannot claim one another's addresses.
- The allocation/deallocation, in-chunk pointer arithmetic, and chunk `Send`
  proofs are recorded in the exact unsafe inventory and safety ledger. Focused
  arena/heap tests, the full collector check, and Miri all pass.

### C2A.3 Completion

C2A.3 completed on 2026-08-21:

- Every run now begins with a valid integer-only 64-byte header initialized
  before its chunk is published. A nonzero 64-bit class ID and compact checked
  geometry fit in that header; canonical type metadata remains C2B's
  responsibility.
- Exclusive run initialization revalidates geometry, clears allocation,
  allocation-word lease, and mark bitmap ranges, and publishes the class
  header only after all rejection points. Repeated, invalid, or out-of-range
  initialization leaves the prior header and class publication unchanged.
- Private checked owner lookup validates arena range, run header, class,
  reconstructed geometry, and exact slot-start offset in that order. It
  rejects header and bitmap bytes, alignment padding, slot interiors, free
  runs, run ends, and addresses owned by another heap.
- Tests cover all empty headers, poisoned-to-zero side metadata, first/last
  slots, adjacent run and chunk topology, independent class identities, and
  failure atomicity. The complete collector check, exact unsafe audit, and
  Miri all pass. Payload allocation remains disabled until C2C.

## Phase C2B — Type Metadata and Per-Heap Allocation-Class Discovery

Execute C2B as three independently verified checkpoints:

- **C2B.1 — canonical object metadata.** Add the process-wide `TypeId`
  discovery registry, canonical `&'static ObjectMetadata` identity, Rust and
  requested slot layout, and monomorphized erased trace/drop dispatch. Replace
  C1's debug prototype records with canonical metadata pointers, but do not
  create heap-local classes or publish runs.
- **C2B.2 — heap-local allocation classes.** Derive geometry before
  publication, add the metadata-pointer-to-dense-class table, and return a
  reusable typed `AllocationClass<T>` carrying heap provenance. Concurrent
  first discovery must publish one class per heap and metadata identity.
- **C2B.3 — typed-run integration.** Publish initialized runs into their
  class pools, enumerate them from heap-owned state, and resolve checked slot
  addresses through the run's dense class ID to canonical trace/drop/layout
  metadata. C2C still owns payload allocation and thread-local leases.

Run focused tests, the exact unsafe inventory, and Miri after each checkpoint.
Failure-injection tests must establish that metadata, class, and run
publication each remain all-or-nothing before the next layer is added.

- Define immutable object metadata containing layout, visitor dispatch, and an
  optional erased `Drop` operation. Intern exactly one descriptor per Rust type
  in a process-wide registry keyed on `TypeId`, and use the winning
  `&'static ObjectMetadata` address as the operational type identity. `TypeId`
  is a cold discovery key, not a run-header field or hot-path comparison. Do
  not add relocation operations while moving collection is deferred.
- Store both the Rust payload layout and an optional requested total slot
  extent in `ObjectMetadata`. The request is the whole pre-alignment slot size,
  not bytes added to the payload; it does not change Rust alignment or select a
  run size. Generic const evaluation rejects an extent below
  `size_of::<T>()`. Because metadata remains canonical per `TypeId`, one Rust
  type has one requested slot-size policy; callers needing another policy use a
  distinct wrapper type. For the bootstrap collector this policy is an
  associated constant on the unsafe managed-representation contract, so it is
  fixed by the same implementation which proves the type's trace layout. The
  heap-local allocation class rounds it to payload alignment and derives slot
  geometry for the collector's one fixed run size. Any remaining unsupported
  result fails class creation before a class ID or run is published.
- Give each typed run one metadata/allocation-class identity in its header;
  ordinary slots contain payload only, with no GC header, metadata pointer,
  mark byte, or finalizer byte.
- Keep every typed run owned and directly enumerable by its heap for its entire
  lifetime. A thread-local allocator may hold exclusive allocation leases over
  integral ranges of that run's allocation-bitmap words; several thread caches
  may therefore allocate from nonoverlapping ranges in one run. Run-side lease
  metadata records which bitmap-word ranges are unavailable to the synchronized
  class slow path, but the run is never owned or discoverable only through
  thread-local state. A cursor needs no separate active versus parked state:
  the owning thread's recursive mutator depth says whether it is currently
  usable, while exclusive collector admission proves that no thread cache is
  in use. A range lease never becomes reachability for the run's objects.
- Maintain a per-heap map from canonical metadata pointer to
  `AllocationClassId` for first-use class discovery and a stable dense class
  table containing that metadata pointer and typed-run pools. Metadata function
  bodies are monomorphized for `T`; the metadata address is the canonical Rust
  type identity while the dense class entry is the canonical heap-local
  allocation identity.
- Return a reusable `AllocationClass<T>` handle after discovery. Its heap
  provenance, metadata pointer, and dense class ID make subsequent worker
  allocation independent of `TypeId` lookup or hashing.
- Because `Mutator::alloc` is safe, an allocation-class handle from another
  heap must be rejected in release builds (or made unrepresentable by the
  final API) before it reaches raw run state. Debug assertions may add detail,
  but may not be the safety boundary.
- Serialize concurrent first metadata interning and per-heap class discovery so
  all contenders observe one metadata address and one class/run pool per heap.
  It is acceptable for immutable metadata candidates to be constructed
  redundantly before one entry wins; only the winner is leaked for process
  lifetime, and no run or callback may be published by a loser.
- Treat `needs_drop::<T>()` as an all-or-none property of the homogeneous run.
  There is one destruction mode: if metadata contains `drop`, unreachable
  allocated slots run it later with the finalizer mutator installed.

Verification:

- repeated and concurrent discovery returns one metadata address per `TypeId`
  and one class ID per `(heap, metadata pointer)`;
- the same Rust type in two heaps shares canonical metadata but receives
  distinct heap-local classes;
- every run header resolves to the expected trace/drop/layout metadata;
- heap enumeration finds every run independently of thread-local allocation
  leases;
- repeated allocation through a retained class performs no `TypeId` lookup;
- safe allocation with a class from another heap is rejected before either
  heap's run state changes;
- no-drop and drop types receive the correct metadata without per-slot policy;
  and
- failed or panicking metadata/class construction publishes no partial entry.

### C2B.1 Completion

C2B.1 completed on 2026-08-21:

- `ObjectMetadata` now records canonical Rust layout, an optional requested
  total pre-alignment slot extent, monomorphized erased trace dispatch, and
  optional erased drop dispatch. `Trace::REQUESTED_SLOT_SIZE` binds the
  allocation policy to the same Rust representation contract; generic const
  evaluation rejects an extent below the representation size, and a wrapper
  type is required for a second policy.
- A process-wide registry uses `TypeId` only for cold discovery. Candidate
  construction occurs outside its mutex; one winning `&'static
  ObjectMetadata` is deliberately retained, concurrent losers are dropped,
  and a construction panic neither publishes an entry nor poisons later
  discovery.
- C1's debug prototype allocation records now compare canonical metadata
  addresses rather than repeating `TypeId` and type-name fields. Focused tests
  exercise exact erased trace and one-time drop dispatch, layout/drop modes,
  repeated and concurrent identity, and recovery after an injected candidate
  panic.
- The collector check, exact unsafe inventory, and Miri pass with 34 unit
  tests. No heap-local class, run publication, payload allocation, or new
  reclamation behavior is present; C2B.2 is next.

### C2B.2 Completion

C2B.2 completed on 2026-08-21:

- `Heap::allocation_class<T>()` derives fixed-run geometry from canonical
  metadata before acquiring heap state, then discovers one dense class per
  `(heap, metadata address)`. Unsupported zero-sized, undersized-request, and
  non-fitting layouts return `UnsupportedLayout` without consuming an ID.
- One heap-state mutex now owns the arena, metadata-address index, and dense
  class table. Immutable candidates are constructed outside that mutex;
  vector and map capacity is reserved before the winning entry and index are
  published together.
- The reusable typed `AllocationClass<T>` retains its heap provenance,
  canonical metadata address, and dense ID. Its strong heap reference is not a
  cycle because heap state stores only dense entries, never handles; it is not
  a managed root and retains no payload allocation.
- Focused tests prove repeated and concurrent same-heap discovery, shared
  metadata but distinct provenance across heaps, no partial class after
  invalid geometry or an injected construction panic, and `Send + Sync` typed
  handles. The collector check, exact unsafe inventory, and Miri pass with 38
  unit tests. C2B.3 is next.

### C2B.3 Completion

C2B.3 completed on 2026-08-21:

- Every class entry now owns an authoritative `RunLocation` pool. A run header
  stores only its heap-local dense class ID and geometry; checked resolution
  validates exact slot position, class geometry, and pool membership before
  returning the class's canonical metadata.
- Run publication reuses an empty heap-owned run or initializes a candidate
  chunk before publishing that chunk. The class pool reserves capacity first,
  so successful arena publication is followed by an infallible location write
  under the same heap-state mutex. Failure adds neither a typed run nor a pool
  entry.
- Focused tests cover trace/drop/layout metadata resolution from headers,
  complete heap enumeration across multiple classes and runs, concurrent
  publication into one class pool, and release-semantic rejection of a
  foreign class before state changes. A direct invalid-publication test proves
  that arena failure adds neither a chunk nor a typed run.
- The collector check and exact unsafe inventory pass with 43 unit tests. Miri
  covers the same run/header paths. No slot is allocated or initialized; C2C
  remains solely responsible for replacing the prototype `Box::leak` path and
  introducing worker-local bitmap-range allocation.

## Phase C2C — Worker-Local Bitmap-Range Allocation and Reuse

Execute C2C as five independently verified checkpoints:

- **C2C.1a — indexed chunk ownership.** Add an authoritative fixed-chunk-base
  index beside the owning arena vector and replace linear chunk membership
  scans with masked-base checked lookup. Publish no payload and retain the
  prototype allocator for this independently verified topology change.
- **C2C.1b — synchronized reference allocation and access transition.** Add
  one synchronized payload-allocation path through the class/run machinery.
  Once it initializes arena payloads, remove C1's leaking prototype allocator
  and allocation-history registry, route `debug_assert_access` through checked
  chunk/run/class metadata, and add provisional exact heap teardown.
- **C2C.2 — heap-specific TLS identity and lifecycle.** Introduce the weak
  heap identity, allocation-lease epoch, recursive mutator depth, bounded dense
  class-to-cursor map, whole-cache invalidation, inert eviction, and TLS
  destruction rules without yet making the cached cursor the ordinary payload
  allocation path.
- **C2C.3 — bitmap-range leasing and hot allocation.** Claim disjoint
  allocation-word ranges through the synchronized class slow path, activate
  cached local allocation, and prove that retained-class allocation performs no
  `TypeId` lookup, chunk lookup, hash lookup, or shared synchronization on a
  cache hit.
- **C2C.4 — pressure, panic, teardown, and audit.** Complete batched pressure,
  abandoned-range accounting, initialization/bitmap panic atomicity,
  teardown and panic-race hardening, and the mandatory post-C2C review.

C2C owns the first implementation of `ThreadHeapState`: the heap-specific TLS
entry, recursive depth, allocation-lease epoch, and cursor map work while
collection is still disabled. C3 later connects that existing state to global
mutator admission and collection phases; it must not introduce a competing TLS
representation.

### C2C.1a Indexed Chunk Ownership

- Keep arena chunks in their owning stable-index vector and add an authoritative
  map from aligned chunk-base address to vector index. `RunLocation` continues
  to use the stable vector index; the map is an ownership index, not a second
  owner.
- Because chunk size and alignment are both one fixed power of two, compute a
  candidate chunk base by masking the numeric managed address. Look up that
  base in the mutator heap's map before deriving or dereferencing a run header.
  A missing key rejects a foreign, stale, or arbitrary address without reading
  memory through it.
- Reserve vector and map capacity before publishing a chunk, validate that the
  base is absent, and publish both entries under the heap-state mutex. Failed
  allocation, initialization, or index publication leaves neither an indexed
  chunk nor an owning-vector entry. Fixed aligned chunks cannot partially
  overlap: equal masked bases are the only overlap case.
- Replace `Arena::find_run` and `checked_slot_owner` linear chunk scans with the
  indexed lookup. Preserve numeric validation before provenance-preserving
  pointer derivation and the subsequent exact run, slot, class, geometry, and
  class-pool checks.
- Keep C1's prototype allocator and access registry unchanged during this
  checkpoint. C2C.1a changes only authoritative chunk ownership and checked
  address lookup, keeping allocator and access-liveness changes out of its
  unsafe audit.

Verification for C2C.1a:

- lookup cost is independent of the number and insertion order of live chunks,
  with instrumentation proving one masked-base index lookup and no vector scan;
- first and last candidate slot addresses of several chunks resolve to the
  exact owning chunk and run topology;
- another heap's address, an arbitrary aligned address, header/bitmap/padding
  bytes, and slot interiors all fail before header dereference;
- chunk publication failure leaves the owning vector and base index mutually
  consistent, and heap teardown deallocates each indexed chunk exactly once;
  and
- focused tests, the exact unsafe inventory, and Miri pass before any payload
  allocation or access transition begins.

### C2C.1b Synchronized Arena Allocation and Access Transition

- Add a synchronized reference allocator before the TLS optimization. It
  obtains or creates a typed run through the existing class slow path,
  initializes the payload before publishing its allocation bit, and provides
  the correctness implementation against which C2C.3's local fast path is
  tested.
- After all `Gc<T>` allocations use arena slots, delete
  `prototype_allocations`, `register_prototype`, and the `Box::leak` allocation
  path. Change `debug_assert_access` to use indexed checked owner resolution and
  compare the resolved canonical metadata address with `metadata_for::<T>()`.
  It must not scan allocation history or compare `TypeId` directly.
- This internal debug assertion diagnoses heap membership, exact slot shape,
  and representation. It does not become the release liveness proof and need
  not read a concurrently changing allocation bitmap. I2 still owns
  release-safe public root/value provenance, while unsafe raw access continues
  to require caller-proven liveness.
- Add the provisional non-reentrant heap teardown required while collection is
  disabled: enumerate allocated slots, invoke each class's erased drop exactly
  once when required, and then release the arena chunks. C6 later replaces
  this terminal path with collector-controlled finalization, but no C2C
  checkpoint may regain C1's intentional payload leak.
- Remove Miri's prototype leak exception in the same checkpoint as the final
  `Box::leak` caller.

Verification for C2C.1b:

- first and last allocated slots of several chunks resolve to the exact owning
  chunk, run, dense class, and canonical metadata;
- the synchronized allocator never exposes an uninitialized slot or publishes
  its allocation bit after an injected unwind between reservation and
  publication;
- the former prototype registry and allocation scan no longer exist, and wrong
  heap or representation diagnostics use the indexed arena path; and
- dropping the heap destroys allocated drop-type payloads exactly once, the
  exact unsafe inventory passes, and Miri reports no leak without its former
  exception before TLS cache work begins.

- Require mutator authority and an `AllocationClass<T>` for every managed
  allocation.
- Give each thread a heap-specific cache containing one captured
  `allocation_lease_epoch` and a dense-class-ID map of current
  bitmap-word-range cursors. The thread-local registry is keyed by collector
  heap identity because one host thread may enter more than one heap. The entry
  retains a weak heap identity but no strong heap owner, so it cannot retain a
  dead heap or its arenas. Keeping the weak allocation identity alive prevents
  address reuse from making stale cursor records appear to belong to a newly
  created heap. The heap retains no back-reference to the cache and never walks
  it.
- Validate the heap-specific cache once at outer mutator entry, after mutator
  admission. If its captured epoch differs from the heap's current epoch,
  replace the entire class-to-cursor map with an empty map and capture the new
  epoch. Do not walk or validate individual cursors. Discarding stale cursor
  records is inert: it must not dereference their runs or attempt to return
  leases which the collector may already have reassigned. The epoch remains
  stable for the whole mutator region.
- Never return a range from TLS destruction, cache eviction, or ordinary
  mutator exit. These paths only retain or forget inert cursor records and do
  not dereference the heap. Full collection is the sole reclamation protocol:
  it clears run-side lease metadata directly without finding or walking thread-
  local caches. Correctness therefore has no TLS destructor-order, heap-owned
  cache registry, or cross-thread cache-lock dependency.
- Allocate ordinary slots from the cached bitmap-word range using only its
  worker-local cursor and free mask. Integral word ownership lets that thread
  update the authoritative allocation bits without contending with another
  allocator. Do not look up or hash `TypeId`, acquire a shared lock, or
  increment a shared byte counter on the ordinary hot path.
- On cache miss or range exhaustion, use the class's synchronized slow path to
  claim a nonoverlapping range from a reusable partial fixed-size run or
  reserve a new typed run from an arena chunk. A small run-side lease bitmap,
  separate from the object allocation bitmap, owns allocation-bitmap words
  rather than individual slots. Initialize the range cursor's local free mask
  from the inverse of those authoritative allocation words, with invalid tail
  slots masked out; do not lease a range containing no free slot.
- Initialize the payload completely before setting its allocation bit. A
  reserved but uncommitted slot is not traceable. Panic unwinding returns the
  slot to local free state without invoking `Drop` on uninitialized bytes. Set
  the allocation bit before returning `Gc<T>`. Sharing that pointer through
  ordinary Rust synchronization makes it visible to another mutator; nothing
  about object visibility is deferred until mutator exit.
- On outer mutator exit, leave each reusable range cursor retained in its
  heap-specific thread cache. Cache eviction merely forgets a cursor and leaves
  that range leased until full collection. Releasing mutator admission makes
  the cache quiescent; retaining or forgetting a range does not publish an
  allocation or root its slots. Re-entry may reuse the whole cache after one
  epoch comparison without the shared class-pool slow path.
- Charge allocation pressure in batches when ranges or runs are claimed and
  reconcile unused slots only when full collection revokes all leases. Bound
  the retained class-cursor map, charge forgotten ranges as unavailable
  capacity, and request collection before accumulated abandoned ranges become
  unbounded. The unit of temporary fragmentation is a range rather than a
  partial run, but select the initial range size, cache bound, and pressure
  threshold only after allocation histograms exist.
- Permit a mutex-backed run-turnover path initially. Changing run-pool or local
  cache policy later must not change pointer, trace, or mutator semantics. The
  heap-specific cache is owned by its thread and needs no mutex; ordinary
  allocations through it do not synchronize.
- Do not choose synchronization for possible future debug reads of allocation
  bitmaps in this phase. Such observations are non-semantic and may lag
  allocation activity. Any later implementation must remain data-race-free,
  but need not provide a transactional bitmap snapshot.

Verification:

- allocation from several mutators never overlaps;
- instrumentation proves repeated allocations in a cached class perform no
  hash lookup or shared synchronization;
- nonoverlapping bitmap-word ranges let several mutators allocate from one run
  without overlapping or sharing an allocation word;
- a cache retained after one outer entry is reused by a later entry on the same
  thread after one cache-level epoch comparison and without class-pool
  synchronization;
- a thread cache does not retain its heap, and an epoch mismatch replaces its
  entire cursor map without dereferencing any stale run;
- TLS destruction and cache eviction do not access the heap or return ranges;
- full collection recovers ranges abandoned by cache eviction and thread exit;
- reused slots are correctly reinitialized and marked allocated;
- panic unwinding never exposes uninitialized storage as an object; and
- dropping the heap without collection destroys every allocated drop-type slot
  exactly once for C2C's non-reentrant test payloads. C6B and C6D replace this
  provisional teardown path with collector-controlled mutator finalization and
  terminal teardown before client/production drop types are admitted.

## Phase C3 — Regional Mutators and Stop-the-World Handshake

Execute C3 as four checkpoints over the `ThreadHeapState` established by C2C:

- **C3A — ordinary admission.** Add active-mutator accounting, same-heap
  recursive entry/exit, panic-safe quiescence publication, and cache activation
  without any collector election.
- **C3B — single-heap STW.** Add collection request/coalescing, writer
  commitment, collector election, active-count drain, exclusive phase entry,
  and release back to ordinary admission for one heap.
- **C3C — cross-heap admission.** Add several simultaneously active
  heap-qualified TLS entries and the dependent-admission exception for queued
  writers. Force A-then-B/B-then-A schedules and the already-exclusive target
  case.
- **C3D — finalizer handoff and recovery.** Add the exclusive-to-finalizer-
  mutator transition, follow-up pressure, writer preference around
  finalization, panic unwinding, waiter teardown, and the complete coordination
  audit required by later sweep/finalization phases.

Do not begin a later checkpoint until the preceding state machine and its
forced-order tests pass independently.

- Implement outer `enter`/`exit` admission with a heap phase mutex/condition
  variable or equivalent state machine.
- If admission and allocation slow paths share one `HeapState` mutex, keep
  their fields and transitions separately documented. The mutex belongs to
  arena-chunk, typed-run-pool, class-discovery, and phase state; it is not held
  by a mutator's local bitmap-range cursor. The collector sets its request under
  that mutex, waits for the active count to reach zero, and then has exclusive
  access to allocation state without retaining a mutator-region lock.
- Extend C2C's `ThreadHeapState` with collection admission. It already contains
  that heap's recursive mutator depth, allocation-lease epoch, and bitmap-range
  cursor map. A thread may have several such states active concurrently;
  entering another runtime heap activates its independent state and admission
  count rather than replacing a singular current-mutator slot.
- Support recursive same-heap entry through that heap's thread-local depth
  without incrementing its active-mutator count again.
- Activate the persistent C2C thread cache at the outermost same-heap entry.
  Nested entry reuses that cache. Only outermost exit makes the cache quiescent,
  leaving reusable cursors retained; eviction and exit never return a range.
- Do not maintain a cross-thread active/parked flag for a cursor. Recursive
  depth is local to its owning thread. The collector need not inspect another
  thread's depth: acquiring exclusive mutator admission proves that every
  TLS cache is quiescent. It then revokes ranges by clearing heap-owned run
  lease bitmaps, without inspecting any cache.
- Provide a heap-qualified scoped current-mutator accessor for reviewed
  destructor and runtime-integration code, for example an HRTB closure API
  rather than a borrow which can escape. An unqualified "current mutator" API
  is invalid because several heaps may be active. A destructor invoked by the
  collector sees its finalizer mutator as current for that heap; a same-heap
  public runtime operation therefore re-enters recursively instead of
  acquiring independent admission.
- Permit nested entry into a different heap. It activates a separate cache and
  active-mutator obligation; holding heap A's mutator neither authorizes heap B
  access nor permits a managed edge between them.
- A collection request prevents ordinary new outer entries, then waits for
  every active mutator of that heap to exit. There is one narrow dependent-
  admission exception: a thread already holding another heap's mutator may
  enter a target heap whose collector is requested or queued but has not yet
  acquired exclusive `Collecting` state. Under the target phase lock, either
  the dependent entry increments its active count or it observes that the
  collector is already exclusive and waits; there is no gap between those
  outcomes.
- Encode that distinction in explicit phase/admission state. A bare
  writer-preferring `RwLock` is not, by itself, the admission implementation:
  it cannot distinguish an ordinary new reader from a dependent cross-heap
  entrant which must bypass a merely queued writer.
- Give the collector a privileged collector-to-mutator handoff. After marking
  fixes the dead set, and before releasing exclusive mutator admission, the
  collector acquires one ordinary mutator lease for its own thread. With an
  `RwLock`-shaped barrier this is an atomic write-to-read downgrade; with an
  active-count state machine it increments the active count and publishes
  `Finalizing` while still holding the coordinator lock. There must be no
  interval in which neither collector exclusion nor the finalizer mutator is
  authoritative.
- Exercise this handoff in C3D from a synthetic completed-collection state; C6B
  connects it to a real fixed dead set and destructor queue.
- The collector thread holds that mutator for the complete `Finalizing` phase.
  Releasing exclusive admission makes finalization concurrent by default:
  ordinary workers may acquire their own mutators, while the collector's held
  mutator prevents another collection from beginning.
- A requester which observes `Collecting` or `Finalizing` records follow-up
  pressure on the active collector coordinator; the completed mark does not
  cover allocations made during finalization. Uncommitted heuristic pressure
  remains coalesced. An explicit request or a heuristic decision which commits
  the next collection may queue its exclusive waiter immediately, although the
  collector-held mutator prevents acquisition until finalization ends. The
  coordinator serializes collector ownership; no second trace or sweep starts
  concurrently.
- Once a collection is committed, queuing for exclusive admission may
  deliberately use writer preference. New ordinary mutators then wait behind
  the collection because the heuristic has selected stop-the-world work as the
  runtime's next priority. A dependent cross-heap entry from an already active
  mutator bypasses only a pending writer, not an active collector. This bounded
  exception prevents two threads holding A then B and B then A from deadlocking
  merely because collectors queue on both heaps. Tune commitment rather than
  weakening ordinary writer priority.
- Consequently, an already-admitted mutator must be able to reach its outer
  exit without synchronously depending on a new ordinary outer mutator
  admission. Recursive same-heap and dependent cross-heap entry remain
  available on that thread. Work which truly requires a new worker must either
  establish that admission before commitment, be left scheduled for after
  collection, or keep the heuristic from committing yet. This is a general
  stop-the-world mutator contract, not a finalizer-specific exception.
- Exclusive mark and sweep code may not enter another heap, invoke a callback,
  or wait for foreign runtime work. Thus, a thread holding heap A while waiting
  for an already-active heap B collector cannot form the reverse dependency.
  Mutator-capable finalization begins only after B leaves exclusive collection
  and installs its ordinary finalizer mutator, so it follows the dependent-
  admission rule like any other active mutator.
- Mutators admitted before the request may continue allocating while they
  finish their bounded region; those allocations remain part of the heap which
  the collector sees after the active count reaches zero. A pending request
  must not strand an admitted mutator before it can exit.
- Every successful allocation has initialized its payload and set its
  allocation bit before returning its pointer, independently of mutator exit.
  Outermost exit retains its local bitmap-range cursors, makes the cache
  quiescent by leaving its recursive region, and then decrements the active-
  mutator count. The admission release/acquire edge makes prior allocator
  metadata writes visible to the collector; it is not a Glam value-publication
  or transactional boundary. Exercise this ordering under Loom or
  deterministic barriers.
- Allocation thresholds request collection but do not synchronously collect
  from the middle of a mutator region.
- Allocation pressure during `Finalizing` records follow-up pressure. Before
  commitment it does not block finalizer allocation. If the heuristic commits,
  its writer may block new mutators immediately, but cannot begin collection
  before the finalization queue drains and the held finalizer mutator is
  released.
- Elect exactly one collector; other requesters wait for or observe its epoch.
- Specify panic unwinding from a mutator closure and ensure outer exit still
  publishes quiescence.

Deterministic tests must force:

1. request immediately before a mutator enters;
2. request while one or several mutators are active;
3. same-heap nested entry while a request is pending;
4. last mutator exit racing a second requester;
5. collector-to-mutator handoff racing coalesced collection pressure;
6. a finalizer waiting for a worker mutator before that pressure is committed;
7. commitment of the next collection blocking a new mutator behind its writer;
8. a panicking mutator;
9. two heaps nested on one thread with independent recursive depths and caches;
10. two threads entering A then B and B then A while collectors are pending on
    both heaps, proving dependent admission breaks the wait cycle;
11. a cross-heap entry waiting for an already-exclusive collector without that
    collector reaching into the held heap; and
12. heap drop while collection waiters exist.

Use Loom for the coordination state where feasible. Repeated stress is
supplementary, not proof.

## Phase C4 — External Root Registry

- Implement a shareable root cell registered once with its heap.
- Cloning a root may use ordinary atomic ownership at this external boundary;
  internal `Gc` copies remain free of that cost.
- Publish the registry's weak cell before returning the first root. Collection
  snapshots strong root-cell references under root-registry synchronization,
  releases that synchronization, and retains the snapshot through marking.
  Concurrent clone/drop then changes only the strong count of an already
  registered cell: a successfully upgraded snapshot keeps it live for that
  collection, while a failed upgrade proves no public root remained at that
  instant.
- Root destruction must not acquire an allocator lock or race into premature
  reclamation.
- Root creation from `Gc` is permitted only within a mutator region.
- Safe root creation validates in release builds that the pointer is a live,
  rootable allocation in the mutator's heap. In particular it rejects a
  foreign-heap pointer and any identity in the completed dead/finalization set
  before registering a root.
- Root access enters or requires the correct heap. If the API accepts an
  explicit mutator, a foreign-heap mutator is rejected in release builds before
  the private unsafe `Gc` gateway is invoked; debug assertions only enrich that
  boundary.
- The registry may retain weak root slots and prune them during a pause; it
  must not retain dead root payloads indefinitely.
- Keep the ownership graph acyclic: a public root handle retains both its root
  cell and heap, while the heap registry retains only weak root-cell entries.

Verification forces root creation, cloning, cross-thread sharing, foreign-heap
access rejection, final drop, and registry pruning around every root-snapshot
boundary. A root cloned from an existing root during a pause remains safe; no
new root can arise from an otherwise unreachable bare pointer while mutators
are stopped.

## Phase C5 — Exact Full Marking

- Stop all mutators and snapshot/visit external roots.
- Enumerate runs directly from the heap, never by discovering them through
  thread caches. For the initial full collector, clear every run's allocation-
  range lease bitmap at the start of the exclusive phase and advance one
  heap-wide `allocation_lease_epoch`. On its next outer entry, each
  heap-specific thread cache compares that one epoch and replaces its entire
  class-to-range-cursor map on mismatch; neither the collector nor the mutator
  validates cursors individually. Post-sweep allocation claims fresh ranges
  from the rebuilt run state. Retaining selected hot leases across a collection
  is deferred profiling work.
- Mark through each allocation class's edge visitor; do not derive outgoing
  edges from fixed byte offsets. Immediate/non-edge fields are invisible to the
  collector.
- Mark by run slot in its side bitmap; duplicate visits terminate immediately.
  Use alternating bitmap color or another epoch scheme which does not require
  touching every slot merely to clear an old mark.
- Use an explicit mark stack or queue rather than recursive Rust calls.
- Trace cycles, diamonds, deep chains, wide graphs, and shared logical
  collection spines.
- Validate that every traced pointer belongs to the collecting heap in debug
  and test configurations.
- Maintain enough per-run live summary to recognize a run with no marked slots
  without enumerating its payloads. Marking may touch chunk/run metadata, but its
  graph traversal remains proportional to reachable managed edges.
- Wrap the attempt in an unwind guard. If tracing or mark-work allocation
  panics, discard the worklist, leave every allocation intact, restore a usable
  non-collecting phase, and let the panic continue to its caller. A retry uses
  a fresh epoch, so marks from the abandoned attempt are irrelevant.
- Do not consume roots or other reachability evidence while marking. Commit
  their retirement only after the corresponding collection succeeds.

Verification includes randomized graph comparison against a simple reference
reachability implementation and million-edge depth tests which cannot overflow
the Rust stack. Deterministic hooks panic after zero, one, and many traced edges;
the caller catches the panic, ordinary mutation resumes, and a later full
collection produces the same survivors as a collection which never failed. A
trace which deliberately panics once must succeed on retry without heap-wide
poisoning. Bitmap-color wrap/toggle histories, allocation during the prior
finalization phase, and runs with zero, one, and all slots live receive focused
tests.

## Phase C6 — Sweep, Mutator Finalization, Retry, and Quarantine

Execute C6 as four checkpoints:

- **C6A — no-drop sweep.** Derive dead sets from allocation/mark bitmaps,
  reclaim wholly dead no-drop runs, publish partially live lazy-sweep state,
  and prove storage is not reused before metadata retirement.
- **C6B — finalizer execution.** Detach dead drop-type slots into the non-
  rootable finalization batch, perform the C3D mutator handoff, run ordinary
  Rust destruction outside collector locks, and enforce non-resurrection.
- **C6C — panic and activity.** Add sparse quarantine, deterministic destructor
  panic recovery, safe draining policy, finalizer activity reporting, and
  follow-up collection-pressure publication.
- **C6D — terminal teardown and audit.** Resolve the last-owner drain protocol
  or explicitly restricted fallback, exercise runtime/heap drop, and perform
  the focused unsafe/finalization audit which closes Gate G1.

Each checkpoint must leave the heap in a usable documented phase after every
recoverable panic injected by its own tests.

- Identify unreachable allocations only after marking completes.
- Do not eagerly enumerate every allocated payload. Inspect run summaries and
  bitmaps:
  - a no-drop run with no marked slots is reclaimed wholesale;
  - a partially live no-drop run becomes unswept and recovers dead slots lazily
    when an allocator next acquires it; and
  - a drop-type run computes its dead slots from `allocated & !marked` and
    queues only those slots for immediate destruction.
- Finalizer registration is implicit in homogeneous run metadata. The initial
  design has no per-slot finalizer bitmap and no global finalizer registry.
  Keep allocation bits set, detach the computed dead slots into a
  collector-owned finalization batch, and make those identities non-rootable
  until destruction finishes.
- If the finalization batch is empty, finish run-state publication and release
  exclusive admission without entering `Finalizing`.
- Before releasing exclusive admission, atomically hand the collector thread
  one ordinary mutator lease and enter `Finalizing`. Then release allocator and
  coordinator locks and reopen shared mutator admission. Invoke each erased
  Rust destructor exactly once under that held mutator. Recursive runtime
  operations reuse it, while worker threads may enter independent mutator
  regions concurrently. No collection may begin until the finalization set is
  drained and the collector releases its finalizer mutator.
- Install that mutator in the scoped current-mutator slot before invoking
  `Drop`. This is how ordinary Rust `Drop`, whose signature cannot accept a
  context argument, may allocate through reviewed GC/runtime APIs without
  receiving an escapable mutator reference.
- Support `Drop` for Rust implementation values and embedding-client payloads
  stored in opaque values. This operational cleanup is not a Glam-visible
  finalizer: its thread, relative order, and collection time are unspecified.
- A destructor may allocate new values, evaluate or schedule work, publish
  diagnostics or host events, retain public roots it already owns, and build a
  fresh equivalent of its payload. Every such managed value is a fresh
  allocation outside the completed dead set and is eligible only for a later
  collection.
- Enforce non-resurrection structurally. `Root::from_gc` (or its equivalent)
  rejects every identity in the completed dead set, and an opaque payload has
  no managed handle to its containing allocation. A destructor may inspect its
  Rust payload while `Drop` owns it, but cannot obtain a `Gc` or root for the
  dying allocation. The same rule prevents rescuing another slot in the
  collector-owned finalization batch through a stale internal pointer.
- Track the ordinary case through run allocation bits and the collector-owned
  batch rather than an object header state machine. Successful `Drop` clears
  the slot's allocation bit. A panic records the slot in sparse quarantine
  state, leaves it non-reusable, and never invokes its destructor again. Fresh
  allocations and already-published effects from the destructor remain valid.
- Specify and test how a destructor panic drains or preserves the remaining
  finalization queue. Before resuming the panic, the implementation must leave
  no allocation ambiguously owned and must restore an ordinary heap phase; a
  queued collection may run only after this recovery. The bootstrap should
  prefer quarantining the failed allocation and safely draining the remaining
  queue, while retaining the first panic for propagation.
- Treat a panic or assertion showing that shared allocator metadata may be
  partially mutated as an internal collector defect. Poison or abort only when
  an attempt guard cannot prove a consistent phase, run, bitmap, class-pool,
  and free-slot boundary.
- Reuse reclaimed storage only after destruction and metadata retirement are
  complete.
- Before admitting production mutator-capable drop types, settle terminal heap
  teardown as an explicit C6 checkpoint. The preferred shape begins a final
  drain while a runtime/value-domain owner lease is still strong, so ordinary
  finalizer mutation is valid and creation of a fresh external root can cancel
  terminal teardown. Do not try to reconstruct or resurrect an `Arc` owner
  after its last strong reference has entered `Drop`. If the public ownership
  representation cannot initiate such a drain, keep last-owner teardown a
  restricted non-reentrant path and document which payload families it may
  destroy; it may not silently invoke a mutator-capable production destructor
  without its promised context.
- Expose queued and running finalizers as operational heap activity so runtime
  quiescence and shutdown cannot race their diagnostics, tasks, or host
  effects. A synchronous `collect_full` report completes only after its
  finalization set has reached terminal object states.
- On completion, publish the post-finalization phase and any remaining
  coalesced collection pressure before releasing the collector's mutator
  lease. A writer for an already-committed collection may already be waiting
  and intentionally preventing new mutator admission; it acquires exclusivity
  after the finalizer and other active mutators exit. Otherwise the heuristic
  may reevaluate the pressure at this boundary. Active mutators follow the
  normal safepoint protocol.

Verification uses drop counters, destructors containing ordinary `Arc`,
`Mutex`, `OnceLock`, and opaque host payloads, scoped current-mutator access,
recursive same-heap entry, bitmap-derived destruction, lazy partial-run sweep,
whole-run reclamation, address reuse tests, and Miri checks for stale
references and double destruction. Instrumentation proves a no-drop partial run
is not eagerly scanned. One opaque destructor allocates and publishes a fresh
quine and a diagnostic; the original identity is reclaimed while the published
value survives the next collection. Another schedules work that enters the
same heap from a worker. Uncommitted pressure raised during that finalizer
remains coalesced. A separate deterministic test commits the next collection
during finalization and proves that writer priority blocks a later mutator,
without beginning its trace until the finalizer exits. A panicking opaque
destructor quarantines only its slot; allocations and effects it published
before panicking remain valid, and after the caller catches the panic another
allocation and full collection must succeed.

Gate G1 passes after C6D plus its focused unsafe-code audit.

## Phase C7 — Shared-Pointer and Worker-Shaped Stress

- Exercise roots handed repeatedly between worker threads.
- Exercise many readers of immutable objects under independent mutator
  regions.
- Exercise one thread requesting collection while other threads allocate,
  block on semantic locks, or unwind.
- Confirm that collection never waits on a pointer-local lock.
- Add metrics for mutator entries, recursive entries, pauses, arena chunks,
  typed runs, class-cache hits/misses, traced objects, reclaimed runs/slots,
  lazy sweeps, deferred requests, fixed-run utilization, and partial-run
  fragmentation. Track cold `TypeId`/metadata discovery separately from
  retained-class allocations so the intended hot-path boundary can be
  profiled.

This phase tests collector mechanisms only. It does not imitate Glam scheduler
semantics beyond the shape needed to validate shared values.

## Phase C8 — Tuning Surface and Final Collector Audit

- Expose internal tuning for arena-chunk size, the single fixed run size,
  worker class-cache capacity, and full-collection thresholds without exposing
  collector jargon as a stable Glam public API. Comparing candidate fixed run
  sizes may require separate heap construction or builds; C8 does not
  introduce variable-size runs. Report bitmap bytes and internal fragmentation
  by metadata-requested slot stride so the value layer can choose its own type
  layouts and size policy from evidence.
- Make an explicit collection report suitable for tests and future runtime
  metrics.
- Audit every unsafe block against `SAFETY.md`.
- Run Miri, Loom, sanitizers, randomized graph tests, stress tests, and the full
  repository checks.
- Record performance and pause measurements without converting unstable
  measurements into brittle unit tests.

The subcrate is ready for production enablement only when the integration plan
also reaches Gate G2.

## Collector Verification Commands

Exact commands may evolve with the crate layout, but the plan requires
equivalents of:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p glam-gc
cargo test -p glam-gc --features deterministic-test-hooks
cargo +nightly miri test --package glam-gc --lib --all-features
cargo test --workspace -q
```

Run supported address/thread sanitizers in CI or a documented local script.
The crate-local verification script remains the authoritative focused entry
point and must evolve when C1 replaces C0's blanket unsafe prohibition. The
workspace command is additional evidence that the path crate and root package
still coexist; it does not replace the focused Miri, Loom, sanitizer, or unsafe
inventory runs. Every concurrency defect receives a forced-order regression
before repair.

## Collector Completion Criteria

- Pointer copying and reading acquire no pointer-local collector lock.
- One heap supports multiple concurrent mutators and shared roots.
- Full collection is exact, non-moving, and cycle collecting.
- Every managed allocation fits one slot in the documented fixed-size typed-run
  geometry; unsupported layouts are rejected without a hidden fallback.
- Unsafe contracts and copied-code provenance are auditable in the subcrate.
- No collector API depends on Glam `Value`, scheduling, reflection, or host I/O.
- Moving, generational, and concurrent collection remain separate future
  designs rather than completion claims of this plan. The visitor and mutation
  gateways must not obstruct them, but the initial collector carries no
  generation or remembered-set machinery merely in anticipation.

Trace derive macros remain deferred. Reconsider one only if the Glam integration
inventory demonstrates substantial mechanical visitor repetition, and treat it
as an independently audited maintenance tool rather than a collector gate.
