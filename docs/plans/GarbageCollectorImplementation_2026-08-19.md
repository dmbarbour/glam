# Glam GC Subcrate Implementation Plan — 2026-08-19

Status: in progress; Phases C0 through C6A.0 are complete, including the C2C.6
verification follow-up. The mandatory post-C1, post-C2C, post-C3E, post-C4,
and post-C5 downstream reviews are complete. C6A.1 is next.

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
| C2C.1a | completed | indexed chunk ownership and checked owner lookup |
| C2C.1b | completed | synchronized arena allocation and prototype access transition |
| C2C.2 | completed | heap-specific TLS identity, epochs, and cache lifecycle |
| C2C.3 | completed | allocation-word leasing and worker-local hot allocation |
| C2C.4 | completed | pressure, panic, teardown, and allocator audit |
| C2C.5a | completed | atomic hierarchical lease-word claiming |
| C2C.5b | completed | typed-run-publication collection pressure |
| C2C.5c | completed | stable class run frontier and lock-free cursor refill |
| C2C.5d | completed | explicit thread-local cache release without eager pruning |
| C2C.6 | completed | forced concurrent exhausted-frontier verification |
| C3A.1 | completed | coordinator state and mutator-gated class discovery |
| C3A.2 | completed | prepare/admit/activate TLS integration and recursion |
| C3A.3 | completed | visibility proof, forced schedules, and Loom model |
| C3B | completed | collector election and single-heap STW quiescence |
| C3C | completed | cross-heap dependent admission |
| C3D | completed | finalizer handoff, pressure, panic, and teardown races |
| C3E | completed | entry-serviced collection and coordinator simplification |
| C4A | completed | checked direct-root construction and access |
| C4B | completed | weak registry publication and stable root traversal |
| C4C | completed | concurrent root lifetime and boundary audit |
| C4D | completed | mutator-scoped allocator capability and ownership migration |
| C5A.0a | completed | mechanical coordinator and managed-data lock split |
| C5A.0b | completed | atomic request and data-side acknowledgement |
| C5A.0c | completed | split-state forced-order audit |
| C5A.1 | completed | clear-before-mark bitmap operations |
| C5A.2 | completed | checked collector lookup |
| C5A.3 | completed | failed mark-attempt invalidation |
| C5B.1 | completed | stable root seeding |
| C5B.2 | completed | checked non-recursive graph marking |
| C5C.1 | completed | trace and worklist panic recovery |
| C5C.2 | completed | invalid-edge recovery and retry |
| C5D.1 | completed | successful mark publication and report |
| C5D.2 | completed | reachability oracle, scale, and verification closeout |
| Post-C5 review | completed | completed C5 audit and downstream-plan reconciliation |
| C6A.0 | completed | post-mark collection-pipeline handoff |
| C6A.1 | pending | dead-set classification without reuse |
| C6A.2a | pending | allocation-lease revocation and epoch publication |
| C6A.2b | pending | class frontier and run-pool retirement |
| C6A.2c | pending | wholly dead no-drop runs and free-run reuse |
| C6A.3a | pending | eager partial no-drop sweep |
| C6A.3b | pending | swept allocator publication |
| C6A.4 | pending | assigned-run pressure |
| C6B.1 | pending | finalization batch and non-rootability |
| C6B.2 | pending | finalizer handoff and destruction |
| C6B.3 | pending | non-resurrection and successful completion |
| C6C.1 | pending | destructor panic, draining, and quarantine |
| C6C.2 | pending | finalizer activity, reports, and pressure publication |
| C6D.1 | pending | terminal-teardown decision and fixtures |
| C6D.2 | pending | terminal teardown |
| C6D.3 | pending | Gate G1 audit |
| C7A | pending | shared-root and immutable-reader stress |
| C7B | pending | allocation and coordinator stress |
| C7C.1 | pending | collection and finalization metrics |
| C7C.2 | pending | allocation and cache metrics |
| C7C.3 | pending | metric consistency audit |
| C8A | pending | tuning and reporting boundary |
| C8B.1 | pending | measurement harness |
| C8B.2 | pending | geometry and workload measurements |
| C8B.3 | pending | paged array tracing exploration |
| C8C | pending | final collector audit |

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
pub struct Allocator<'m, T: Trace> { /* mutator-scoped typed allocation */ }

impl Heap {
    pub fn with_mutator<R>(&self, f: impl for<'h> FnOnce(&Mutator<'h>) -> R) -> R;
    pub fn request_collection(&self);
    pub fn collect_full(&self) -> Result<CollectionReport, CollectionError>;
}

impl Mutator<'_> {
    pub fn allocator<T: Trace>(&self) -> Result<Allocator<'_, T>, UnsupportedLayout>;
}

impl<T: Trace> Allocator<'_, T> {
    pub fn alloc(&self, value: T) -> Gc<T>;
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

`Allocator<'m, T>` is a scoped capability borrowed from the admitted
`Mutator`, not an owning description of heap topology. It cannot be returned
from `Heap::with_mutator`, stored in a managed value, or used after that
mutator region. The heap retains canonical metadata, dense class identities,
run pools, and stable frontier state. The scoped allocator may retain a stable
frontier cell for its borrow but never an `Arc<HeapInner>`. Internal
per-thread/per-heap state may cache class IDs or frontier cells across regions
to avoid repeated cold discovery; that cache is weak/inert with respect to heap
ownership and is not a public capability.

`Root<T>` keeps its root cell alive but does not own `Heap`. Access always
requires a live matching `Mutator`; dropping the value domain makes any escaped
root inert rather than extending the domain's lifetime.

`request_collection` is idempotent and nonblocking. It may be called outside a
mutator or from inside one; an active mutator only records the request. A
requested collection begins when a later outer entry finds that heap idle, not
as work performed by outer exit. `collect_full` is the synchronous maintenance
boundary and rejects a call from a thread currently holding a mutator for any
heap rather than entering a possible cross-heap wait. Both are Rust embedding
operations, not Glam evaluation effects.

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
- Treat the header and all three side bitmaps as one metadata prefix. Round the
  first payload slot to the next 128-byte boundary or the payload's stricter
  Rust alignment. The header remains 64 bytes; the purpose is to keep metadata
  and payload storage in separate 128-byte regions, not to give the header its
  own region or assert a universal hardware cache-line size.
- Consume I0's preliminary production layout inventory when selecting the
  provisional run size. If I0 is not yet complete, record equivalent
  representative layout measurements here and reconcile them with I0 before
  any production ownership migration.
- Store allocation, allocation-word leases, and mark state in run-side
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
  path because the planned `Allocator<'_, T: Trace>` keeps its implicit
  `Sized` bound;
- pointer ownership checks distinguish heaps in debug/test builds; and
- Miri and property tests cover mask arithmetic, alignment, and the maximum
  supported address range.

### C2A.1 Completion

C2A.1 completed on 2026-08-21:

- The provisional fixed run size is 64 KiB. It bounds cold-class waste to one
  modest run while giving the I0 representative 16-, 24-, 32-, 144-, 192-,
  and 256-byte layouts respectively 4,024, 2,698, 2,028, 453, 340, and 255
  slots. Allocation, lease, and mark side metadata consumes about 1.55%,
  1.06%, 0.79%, 0.21%, 0.16%, and 0.11% of those runs. A deliberately tiny
  one-byte class remains supported with about 20.1% bitmap overhead rather
  than defining value-layer alignment policy in the collector.
- Pure checked geometry derives allocation, allocation-word lease, and mark
  bitmap ranges, aligned first-slot offset, stride, count, and exact slot
  indices. Binary search selects the greatest fitting slot count without
  allocating or mutating heap state.
- The geometry exposes a minimally aligned maximum managed size of 65,408
  bytes and maximum supported power-of-two alignment of 32 KiB. Larger or
  overflowing layouts, zero-sized payloads, and undersized slot requests fail
  derivation without state.
- Focused boundary and sampled-layout tests, the exact unsafe audit, the full
  collector check, and Miri all pass. C2A.1 adds no unsafe code or arena
  allocation.
- A 2026-08-22 layout-hardening spike subsequently aligned the first payload to
  the next 128-byte metadata boundary. Tests latch both ordinary and stricter
  payload alignments and reject reconstructed geometry which violates that
  separation. Bitmap order remains allocation, lease, then mark; it has no
  semantic or independently claimed cache-locality significance.

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
  publication and add the metadata-pointer-to-dense-class table. The original
  checkpoint returned a reusable typed `AllocationClass<T>` carrying heap
  provenance; C4D deliberately replaces that public ownership shape with a
  mutator-scoped `Allocator<'_, T>` while retaining the heap-owned dense class.
  Concurrent first discovery must publish one class per heap and metadata
  identity.
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
  lifetime. A thread-local allocator may hold an exclusive lease over one of
  that run's allocation-bitmap words; several thread caches may therefore
  allocate from different words in one run. Run-side lease metadata records
  which bitmap words are unavailable to the synchronized
  class slow path, but the run is never owned or discoverable only through
  thread-local state. A cursor needs no separate active versus parked state:
  the owning thread's recursive mutator depth says whether it is currently
  usable, while exclusive collector admission proves that no thread cache is
  in use. A word lease never becomes reachability for the run's objects.
- Maintain a per-heap map from canonical metadata pointer to
  `AllocationClassId` for first-use class discovery and a stable dense class
  table containing that metadata pointer and typed-run pools. Metadata function
  bodies are monomorphized for `T`; the metadata address is the canonical Rust
  type identity while the dense class entry is the canonical heap-local
  allocation identity.
- Return a typed `Allocator<'m, T>` whose lifetime is derived from the admitted
  `&'m Mutator`. It carries the dense class ID and access to the stable frontier
  needed for fast allocation, but it does not own the heap and cannot escape
  the mutator region. Repeated allocation through one scoped allocator remains
  independent of `TypeId` lookup or hashing.
- Discover a new or existing heap-local allocation class only through an
  admitted `Mutator`. Heap-owned class-table and metadata-index topology is
  therefore frozen when collection drains all mutators, without introducing a
  separate topology admission counter. A private per-thread/per-heap cache may
  retain dense IDs and frontier cells for later mutator regions; it retains no
  strong heap owner and exposes no allocator capability.
- Because `Allocator<'m, T>` is constructed from one admitted mutator and
  performs allocation itself, a foreign-heap class handle is not representable
  in the public API. Internal cached identities remain checked at their narrow
  heap boundary; debug assertions may add detail but are not the only safety
  boundary.
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
- class discovery cannot enter while an exclusive collector owns the heap,
  recursive same-heap mutator entry may discover a class, and a retained class
  remains usable after its discovery region exits;
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

This completion records the ownership shape implemented at that checkpoint.
C4D intentionally removes the reusable, `Send + Sync`, heap-owning handle and
migrates the same heap-owned class topology behind a mutator-scoped allocator.

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
  introducing worker-local allocation-word leasing.

## Phase C2C — Worker-Local Allocation-Word Leasing and Reuse

Execute C2C as nine independently verified checkpoints:

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
- **C2C.3 — allocation-word leasing and hot allocation.** Claim disjoint
  allocation words through the synchronized class slow path, activate
  cached local allocation, and prove that retained-class allocation performs no
  `TypeId` lookup, chunk lookup, hash lookup, or shared synchronization on a
  cache hit.
- **C2C.4 — pressure, panic, teardown, and audit.** Complete batched pressure,
  abandoned-word accounting, initialization/bitmap panic atomicity,
  teardown and panic-race hardening, and the correctness-baseline audit.
- **C2C.5a — atomic hierarchical lease claiming.** Replace repeated per-
  allocation-word lease probes with lease-word scanning and atomic bit claims,
  independently of class run selection.
- **C2C.5b — typed-run-publication collection pressure.** Remove allocation-
  word and leased-capacity pressure accounting. Treat each successful typed-
  run publication as one automatic pressure event, with a provisional
  allowance of one chunk-equivalent of runs.
- **C2C.5c — stable class run frontier.** Publish a stable current-run record
  per class so ordinary cursor refill can claim from it without the heap-state
  mutex, retaining that mutex only for exhausted-frontier advancement and run
  publication.
- **C2C.5d — explicit TLS release.** Remove automatic whole-registry pruning
  from mutator entry and provide an explicit current-thread cleanup operation,
  then perform the mandatory post-C2C review.

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

#### C2C.1a Completion

C2C.1a completed on 2026-08-22:

- `Arena` keeps its chunks in the stable owning vector and now maintains one
  authoritative `HashMap` from each aligned 8 MiB base to that vector index.
  Fixed size and alignment make equal bases the only overlap case.
- Chunk publication checks absence and reserves both vector and map capacity
  before changing either logical collection. The candidate remains uniquely
  owned and is dropped on every preceding failure; successful insertion occurs
  while the arena is exclusively borrowed under heap state.
- `find_run` and checked slot-owner recovery mask an integer address to one
  candidate chunk base, perform one index lookup, validate the resulting range,
  and only then derive a run pointer from the owning allocation. Typed-run
  selection for allocation remains the deliberate C2B slow path.
- Focused instrumentation proves one ownership-index lookup independent of
  live chunk count and query order. Tests cover exact first/last slots across
  several chunks, foreign and arbitrary addresses, non-slot regions, and
  vector/index consistency after rejected publication.
- The collector check passes with 45 unit tests and six compile-fail doctests;
  the exact unsafe inventory and Miri pass unchanged. Payload allocation and
  the prototype access registry remain untouched for C2C.1b.

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

#### C2C.1b Completion

C2C.1b completed on 2026-08-22:

- At this historical checkpoint,
  `Mutator::alloc(&AllocationClass<T>, T)` was the synchronized correctness
  allocator. C4D moves that same path behind `Allocator<'_, T>::alloc(T)` and
  makes foreign-class construction unrepresentable at the public boundary.
  The allocator searches the class's authoritative run pool, publishes a typed
  run only when needed, and holds heap state through unique slot selection,
  payload initialization, and allocation-bit publication.
- Slot selection itself publishes nothing. A deterministic hook panics after
  selection and proves the allocation bit remains clear and the input value is
  destroyed normally. Production performs no panicking operation between its
  typed payload write and the allocation bitmap write.
- `Gc<T>` construction now receives only initialized arena pointers. The
  prototype address registry, `register_prototype`, payload `Box::leak`, and
  allocation-history scan are gone. Debug access resolves indexed
  chunk/run/slot/class topology and compares canonical metadata without using
  `TypeId` or promising allocation-liveness checks.
- Provisional terminal heap teardown enumerates allocation bits and invokes
  each homogeneous class destructor exactly once before arena RAII releases
  its chunks. Collection remains disabled, so all payloads stay live until
  this terminal path; reentrant finalization and destructor-panic hardening
  remain C2C.4/C6 work.
- Tests force allocation through every run in a chunk and into later chunks,
  validate boundary topology and values, reject foreign classes before state
  changes, latch pre-publication unwind behavior, and count exact destruction.
  Native tests retain a three-chunk boundary fixture; Miri uses its two-chunk
  form to keep interpretation bounded while exercising the same transition.
- The collector check passes with 49 unit tests and six compile-fail doctests;
  the exact unsafe inventory passes. Miri passes all 49 unit tests with leak
  checking enabled and no prototype exception.

- Require mutator authority and a mutator-scoped `Allocator<'_, T>` for every
  managed allocation.

### C2C.2 Heap-Specific TLS Identity and Lifecycle

- Give each thread a heap-specific cache containing one captured
  `allocation_lease_epoch` and a dense-class-ID map of current
  allocation-word cursors. The thread-local registry is keyed by collector
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
- Never return a word lease from TLS destruction, cache eviction, or ordinary
  mutator exit. These paths only retain or forget inert cursor records and do
  not dereference the heap. Full collection is the sole reclamation protocol:
  it clears run-side lease metadata directly without finding or walking thread-
  local caches. Correctness therefore has no TLS destructor-order, heap-owned
  cache registry, or cross-thread cache-lock dependency.

Use a bounded, hash-free class cursor lookup so C2C.3 does not have to replace
the lifecycle representation when it activates local allocation. Cursor
records remain inert numeric topology until that checkpoint.

Verification for C2C.2:

- same-heap recursive regions balance one TLS depth while nested different
  heaps retain independent cache entries;
- user panic unwinding returns the recursive depth to zero without clearing a
  still-current cache;
- an epoch change leaves the inactive record untouched, then the next outer
  entry clears the entire cursor cache before use and captures the new epoch;
- class-cache capacity remains fixed and a collision forgets only the prior
  inert record;
- TLS keeps only a weak heap identity, a dead heap is collectible before TLS
  destruction, and a later entry prunes the dead record without heap access;
  and
- focused tests, the exact unsafe inventory, and Miri pass before cached
  cursors claim or modify any run-side lease.

#### C2C.2 Completion

C2C.2 completed on 2026-08-22:

- Every host thread now owns a TLS registry keyed by the numeric address of a
  weak `HeapInner` identity. The retained `Weak` keeps a dead Arc allocation's
  identity from being reused while a stale record exists, but cannot retain the
  heap or its arenas. Entering any heap prunes dead, inactive records.
- `Heap::with_mutator` installs an unwind-safe same-thread entry guard.
  Same-heap recursion increments one checked depth; different heaps receive
  independent records. Ordinary outer exit retains current cache state and
  only decrements depth.
- Each heap has a nonzero atomic allocation-lease epoch. Only outer entry
  compares it with the captured epoch; mismatch clears all cursor records
  without inspecting a run or returning a lease. C3 later places real mutator
  admission before this already-defined validation point.
- The cursor cache is a 64-entry direct-mapped array. Dense class ID selects a
  slot and the stored full ID distinguishes collisions, so lookup is bounded
  and hash-free; replacement merely forgets inert numeric cursor topology.
  C2C.3 activates the prepared run/word/free-mask fields.
- Deterministic tests cover recursion, cross-heap nesting, panic balancing,
  whole-cache epoch invalidation, collision eviction, weak heap release, and
  dead-record pruning. The collector check passes with 54 unit tests and six
  compile-fail doctests; the exact unsafe inventory and full leak-checking Miri
  run pass. No allocation yet reads or writes a TLS cursor.

### C2C.3 Allocation-Word Leasing and Hot Allocation

- Allocate ordinary slots from the cached bitmap word using only its
  worker-local cursor and free mask. Integral word ownership lets that thread
  update the authoritative allocation bits without contending with another
  allocator. Do not look up or hash `TypeId`, acquire a shared lock, or
  increment a shared byte counter on the ordinary hot path.
- On cache miss or word exhaustion, use the class's synchronized slow path to
  claim a nonoverlapping word from a reusable partial fixed-size run or
  reserve a new typed run from an arena chunk. A small run-side lease bitmap,
  separate from the object allocation bitmap, owns allocation-bitmap words
  rather than individual slots. Initialize the cursor's local free mask from
  the inverse of that authoritative allocation word, with invalid tail slots
  masked out; do not lease a word containing no free slot.
- Initialize the payload completely before setting its allocation bit. A
  reserved but uncommitted slot is not traceable. Panic unwinding returns the
  slot to local free state without invoking `Drop` on uninitialized bytes. Set
  the allocation bit before returning `Gc<T>`. Sharing that pointer through
  ordinary Rust synchronization makes it visible to another mutator; nothing
  about object visibility is deferred until mutator exit.
- On outer mutator exit, leave each reusable word cursor retained in its
  heap-specific thread cache. Cache eviction merely forgets a cursor and leaves
  that word leased until full collection. Releasing mutator admission makes
  the cache quiescent; retaining or forgetting a word does not publish an
  allocation or root its slots. Re-entry may reuse the whole cache after one
  epoch comparison without the shared class-pool slow path.
- Charge allocation pressure in batches when words or runs are claimed and
  reconcile unused slots only when full collection revokes all leases. Bound
  the retained class-cursor map, charge forgotten words as unavailable
  capacity, and request collection before accumulated abandoned words become
  unbounded. The unit of temporary fragmentation is one allocation word rather
  than a partial run; keep that lease granularity fixed in the initial
  collector and tune the cache bound and pressure threshold only after
  allocation histograms exist.
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
- nonoverlapping bitmap words let several mutators allocate from one run
  without overlapping or sharing an allocation word;
- a cache retained after one outer entry is reused by a later entry on the same
  thread after one cache-level epoch comparison and without class-pool
  synchronization;
- a thread cache does not retain its heap, and an epoch mismatch replaces its
  entire cursor map without dereferencing any stale run;
- TLS destruction and cache eviction do not access the heap or return words;
- cache eviction and thread exit leave their words leased and unavailable;
  C5 verifies that the first full collection clears those leases and advances
  the cache epoch without finding the departed thread;
- every newly selected free slot is initialized before its allocation bit is
  published; actual dead-slot reuse begins with C6 and is verified there;
- panic unwinding never exposes uninitialized storage as an object; and
- dropping the heap without collection destroys every allocated drop-type slot
  exactly once for C2C's non-reentrant test payloads. C6B and C6D replace this
  provisional teardown path with collector-controlled mutator finalization and
  terminal teardown before client/production drop types are admitted.

#### C2C.3 Completion

C2C.3 completed on 2026-08-22:

- Each slow-path claim leases exactly one complete allocation-bitmap word,
  giving one worker sole authority over at most 64 object slots. The cursor
  stores that single word index directly; larger leases are not latent in the
  representation and would require a separately justified design change.
- A mutator obtains one hash-free `ThreadCacheHandle` at region entry. A cache
  hit borrows only its thread-local state, selects a local free bit, writes the
  payload, and publishes the allocation bit. It performs no `TypeId`, chunk,
  class-map, or shared-lock lookup. Re-entering the same heap on the same
  thread retains this cursor after the one outer-entry epoch comparison.
- The synchronized miss path holds heap state while validating the class,
  finding or publishing a typed run, reading an unleased allocation word, and
  setting its lease bit. Distinct workers can then mutate distinct words of
  the same run without data races or allocator contention.
- The fixed 64-entry direct-mapped cache moved its backing array to heap
  storage. This keeps mutator-entry stack usage independent of cache width and
  lets the existing small-stack Loom smoke model continue exercising the
  entry boundary; lookup remains bounded and allocation-free.
- Tests force retained hot-path reuse across mutator regions, exact turnover
  at 64 slots, eight concurrent workers allocating from disjoint words in one
  run, tail-bit masking, collision eviction, thread exit, and the retained
  synchronized reference allocator. Instrumentation proves only cache misses
  increment the shared claim count.
- Forgotten cursors deliberately leave their lease bits set. Recovery cannot
  be tested before collection exists; that latching test now belongs to C5,
  alongside epoch advancement. Likewise, reclaimed-slot reuse belongs to C6.
- The collector check passes with 60 unit tests, the Loom smoke test, and six
  compile-fail doctests. Clippy, the exact unsafe inventory, and full
  leak-checking Miri all pass.

### C2C.4 Pressure, Panic, Teardown, and Allocator Audit

- Record allocation pressure only when the synchronized slow path leases a
  word. Charge the complete free capacity handed to that cursor, not each
  object it later initializes. The hot path must remain free of shared
  counters.
- Do not make eviction or TLS destruction call back into the heap merely to
  classify a lease as abandoned. Claimed capacity is monotonic until a full
  collection revokes every lease, so the same conservative total already
  includes retained, evicted, and departed-thread cursors. C5 resets the total
  and advances the lease epoch together.
- Until allocation histograms can tune policy, set the provisional collection-
  pressure threshold to one arena chunk. Reaching it records a pending request;
  C3B connects that request to collector election. Arithmetic saturates so a
  diagnostic counter cannot unwind after a lease has become authoritative.
- Add a deterministic worker-local hook at the final pre-initialization point.
  An injected unwind must leave the free mask and allocation word unchanged,
  drop the still-owned input normally, and let the same cursor reuse that
  exact slot without another synchronized claim.
- Latch the teardown ownership boundary: an active mutator region necessarily
  exists under an authorized heap owner, so last-owner teardown cannot race
  it. C4D removes `AllocationClass` as an independent owner. A stale TLS class
  or cursor cache remains weak and inert after teardown.
- Keep C2C's terminal destructor path explicitly provisional. It supports the
  non-reentrant, non-panicking fixtures required to avoid leaks while
  collection is disabled. Destructor panic recovery, quarantine,
  mutator-capable finalization, and the final last-owner protocol remain the
  already specified C6B–C6D work; C2C must not invent a competing policy.
- Close C2C with a focused review of the raw allocation path, lease/data-race
  proof, bitmap publication order, weak-cache lifecycle, pressure accounting,
  terminal ownership, exact unsafe inventory, and Miri behavior.

Verification for C2C.4:

- a retained word incurs one pressure charge while 64 small hot allocations
  incur none, and turnover incurs exactly one additional charge;
- evicted and departed-thread words remain included in leased capacity;
- the threshold and saturating arithmetic latch a pending request without an
  allocation-time overflow panic;
- injected unwind immediately before payload initialization republishes
  nothing and the next allocation reuses the selected slot;
- forced owner handoff keeps the heap live through an active mutator, then
  permits terminal teardown only after that worker releases its last owners;
- all allocated non-panicking drop fixtures are destroyed exactly once; and
- focused tests, Loom smoke coverage, exact unsafe inventory, Clippy, and full
  leak-checking Miri pass.

#### C2C.4 Completion

C2C.4 completed on 2026-08-22:

- Heap state now counts claimed words and their leased free capacity only on
  the synchronized claim path. It uses saturating arithmetic and latches a
  pending collection request at one arena chunk. Sixty-four small allocations
  in a retained word add no shared accounting traffic; the sixty-fifth claims
  and charges exactly one more word.
- The charge is intentionally conservative and monotonic. Evicted and exited-
  thread cursors remain included without weak upgrades, TLS callbacks, or a
  second abandoned-word protocol. C5 must clear the lease bitmaps, reset this
  pressure, and advance the cache epoch as one exclusive transition.
- This pressure model is an implemented correctness baseline rather than the
  retained policy. C2C.5b removes it in favor of successful typed-run
  publication, before the ordinary cursor-refill path becomes lock-free.
- A deterministic hook immediately before local payload initialization proves
  that unwind drops the input, publishes no allocation bit, leaves the heap
  mutex usable, and lets the retained cursor reuse the same slot without a new
  claim. The production path invokes no callback there and has only its two
  infallible publication writes afterward.
- A forced owner-handoff test originally held a worker inside its mutator after
  the initiating `Heap` and class handles were dropped. C4D replaces the class-
  owner portion of that fixture: the admitted region's authorized heap owner
  alone keeps the domain live, while its scoped allocator neither owns nor
  outlives that region.
- Review found no reason to duplicate C6's destructor-panic and mutator-
  finalization design. C2C's provisional terminal path remains explicitly
  limited to non-reentrant, non-panicking payload destruction, which is enough
  to keep the pre-collector implementation leak-free.
- The final C2C correctness-baseline verification passes 63 unit tests, one
  Loom smoke model, and six compile-fail doctests. Formatting, Clippy with
  warnings denied, the exact unsafe inventory, and full leak-checking Miri all
  pass.

### C2C.5 Lock-Free Lease Claims, Run Pressure, Class Frontier, and TLS Release

C2C.5 is a performance extension over the completed C2C correctness baseline.
It must preserve one authoritative lease bit per allocation word, worker-local
non-atomic allocation-word mutation after a successful claim, inert TLS
eviction, and collector-only lease revocation. Allocation-word ownership must
cease to participate in collection-pressure accounting before the class
frontier makes ordinary claims lock-free.

#### C2C.5a Atomic Hierarchical Lease-Word Claiming

- Treat every published lease-bitmap word as `AtomicU64`. Initialization may
  write the enclosing untyped run before publication, but after publication
  every read, claim, reset, and diagnostic observation of lease words uses the
  atomic representation. Allocation and mark bitmap words remain ordinary
  `u64` storage under their existing exclusive ownership rules.
- Make the layout assumptions compile-time explicit. Assert that
  `size_of::<AtomicU64>() == size_of::<u64>()`, and separately assert that the
  bitmap-word stride and run metadata boundary satisfy
  `align_of::<AtomicU64>()`. Do not require `u64` and `AtomicU64` to have equal
  alignment: Rust may align the atomic more strictly. Initialize lease words
  as `AtomicU64` in their raw arena storage rather than publishing a live
  `u64` and relying on an in-place type reinterpretation.
- Scan the lease bitmap itself rather than iterating allocation words. One
  lease word summarizes up to 64 allocation words, so the provisional 64 KiB
  run requires at most roughly sixteen probes even for the densest supported
  slots.
- For each ordinary lease word, calculate candidate bits directly from the
  inverse observed word. Do not construct a valid-bit mask on every retry.
  After selecting the first zero bit, derive its absolute allocation-word
  index. Only the final lease word can produce an index at or beyond
  `lease_bitmap.bit_len`; because those invalid bits form a suffix, observing
  such an index means that lease word has no valid candidate left.
- Claim the selected bit with a compare-exchange loop. A failed CAS recomputes
  candidates from the newly observed word; a successful CAS grants that worker
  exclusive access to the corresponding non-atomic allocation word. Review
  and document memory ordering together with run publication and C3's mutator
  admission rather than relying on atomicity alone for pointer initialization
  visibility.
- After winning a bit, read the allocation word and construct its exact free
  mask using the existing allocation-tail rule. If no free slot remains, keep
  the lease bit set and continue; never return or race to clear it. Once C5
  rebuilds leases, it should preset lease bits for completely full allocation
  words so this fallback is exceptional rather than ordinary.
- Keep the one-word `AllocationCursor` contract unchanged. This checkpoint
  replaces only candidate discovery and ownership acquisition, not hot payload
  initialization, pressure charging, TLS retention, or lease granularity.

Verification for C2C.5a:

- compile-time assertions reject any target on which the shared bitmap
  geometry cannot represent correctly aligned `AtomicU64` lease words;
- deterministic geometry tests cover one lease word, several lease words, and
  a partial final lease word without ever claiming an out-of-range bit;
- many synchronized workers race on one run and obtain every claimable
  allocation-word index at most once, with no heap mutex protecting the CAS;
- a deliberately full allocation word is claimed, recognized as unusable, and
  left unavailable while search continues;
- instrumentation proves the search reads lease words rather than rereading
  one lease word for each represented allocation word;
- all post-publication lease accesses are atomic, as checked by the exact
  unsafe audit and a focused ThreadSanitizer or equivalent race run in addition
  to Miri; and
- a small Loom model exercises the compare-exchange state transition even if
  the raw arena-pointer integration remains covered by native forced schedules.

#### C2C.5b Typed-Run-Publication Collection Pressure

- Remove `AllocationPressure`'s claimed-word count, leased-capacity byte total,
  and `record_claim` call. Do not replace them with atomics: claiming, retaining,
  forgetting, or revoking an allocation word is not itself a collection-
  pressure event.
- Treat each successfully published typed run as exactly one automatic
  allocation-pressure event in the initial collector. This applies whether
  the run uses an empty slot in an existing chunk or is the first run in a new
  chunk. Chunk allocation itself emits no second event. A failed candidate-run
  initialization or publication emits no event because no typed run became
  authoritative.
- Keep a saturating typed-run-publication count and latched request under heap
  state. The initial allowance is `RUNS_PER_CHUNK`, currently 128 publications
  for 64 KiB runs in an 8 MiB chunk. The publication which reaches 128 latches
  one request; C3B later owns election and coalescing. This normally permits an
  outer mutator exit to service collection before allocation needs a second
  chunk, without charging individual leases or slots.
- Permit an allocating mutator to publish the run which crosses its allowance
  and to continue allocating until its outermost mutator exit. The request is
  already latched, but the initial STW design has no mid-region safepoint. A
  long mutator can therefore overshoot by additional runs or even a chunk;
  headroom and mid-mutator safepoints remain later tuning rather than hidden
  complexity in the first policy.
- Abandoned word leases need no side counter or TLS callback. They consume
  capacity already represented by a typed run and can induce future run
  publication naturally. That publication supplies the pressure event. Full
  collection revokes the leases regardless of how they became unreachable
  from a thread cache.

Scope boundary: C2C.5b ends after latching and inspecting its provisional
automatic request. It does not service collection, implement an explicit
request API, publish sweep results, or account for finalizer allocation. Those
requirements are stated under the owning C3 and C6 phases below.

Verification for C2C.5b:

- any number of word claims, cursor turnovers, evictions, and thread exits
  changes no pressure state unless it ultimately requires a new typed run;
- every successful typed-run publication increments pressure exactly once,
  including the first run in a newly allocated chunk, without an additional
  chunk-publication charge;
- 127 successful publications leave the automatic request clear, while the
  128th latches exactly one request and later publications leave it coalesced;
- candidate chunk allocation, run initialization, overlap, and publication
  failure add neither an authoritative typed run nor a pressure event;
- abandoned leases can force later run publication without requiring direct
  classification or double-counting;
- pressure observation and mutation remain under the existing heap-state lock,
  with no atomic counter added to the lease-claim path; and
- focused threshold, failure-atomicity, and existing allocator tests, Clippy,
  the exact unsafe inventory, Miri, and available sanitizer checks pass before
  the lock-free class-frontier checkpoint begins.

#### C2C.5c Stable Class Run Frontier and Lock-Free Refill

- Give each heap-local allocation class stable shared allocation state owned by
  its authoritative heap entry and borrowed or temporarily retained by scoped
  allocators, without a back-reference to the heap. Keep mutable run-pool
  ownership under heap state, but publish the current candidate through an
  atomic pointer to a heap-owned, stable run record containing both
  `RunLocation` and `RunAddress`.
- A cursor miss first loads that run record with publication ordering and calls
  C2C.5a's atomic lease claimant directly. This path performs no heap-state
  lock, class-vector lookup, run-vector scan, chunk lookup, or `TypeId` lookup.
- If the current run has no claimable lease bit, enter the heap-state slow
  path. Recheck the frontier after acquiring the mutex because another worker
  may already have advanced it. Otherwise advance monotonically to an existing
  run or publish one new typed run, then release-publish its stable record.
  Never rescan already exhausted prefix runs before collection.
- Run records remain valid for the heap lifetime in the initial collector.
  Before C6 ever repurposes or retires a run, exclusive collection must first
  invalidate every atomic class frontier which could name it. A stale class
  frontier may never point at storage already reassigned to another class.
- During full collection, rebuild each run's atomic lease words so full
  allocation words are preset unavailable, reset each class frontier to a run
  with claimable capacity (or no run), and then advance the one heap-wide lease
  epoch before releasing mutator exclusion. Typed-run-publication allowance is
  the separate C2C.5b/C6 policy. TLS cache records remain invalidated wholesale
  on their next outer entry.

Verification for C2C.5c:

- repeated cursor refills in one class acquire only atomic lease state until
  the current run is exhausted;
- racing workers share one current run, claim distinct words, and cause at most
  one frontier advance or new-run publication per exhaustion boundary;
- instrumentation proves the class's exhausted run prefix is not rescanned;
- run publication failure exposes neither a frontier record nor a class-pool
  entry, while a losing publisher observes and uses the winner;
- instrumentation proves atomic claims and refills from the published frontier
  do not update or observe collection-pressure state, while the synchronized
  slow path charges exactly one event after it publishes a new typed run;
- epoch invalidation, weak TLS teardown, terminal heap destruction, and
  multi-heap nesting retain their existing behavior; and
- focused tests, forced schedules, Clippy, the exact unsafe inventory, Miri,
  and available sanitizer checks pass before TLS cleanup changes begin.

#### C2C.5d Explicit Thread-Local Cache Release

- Remove the `HashMap::retain` scan from `ThreadHeapEntry::enter`. Entry into a
  known heap performs only its direct thread-local key lookup and normal epoch
  validation. Entry into a new heap inserts one record without examining
  unrelated records.
- Do not opportunistically classify or prune dead heaps. The expected runtime
  heap lives for nearly the complete process, while the final dead heap would
  never be discovered by a subsequent entry anyway. Eager scanning therefore
  adds recurring work without providing a reliable reclamation boundary.
- Retain each dead record until its host thread exits or explicitly releases
  its cache registry. Its `Weak<HeapInner>` keeps the old Arc allocation address
  unavailable for reuse while the numeric TLS key and inert run pointers
  remain present, but does not retain the dead heap, arenas, or payloads.
- Add a provisional associated operation such as
  `Heap::release_current_thread_caches() -> usize`. It acts only on the calling
  thread and returns the number of discarded heap records. The name may be
  narrowed with the public API later, but cleanup must not require a live
  handle to a heap which may already be dead.
- Require that the calling thread hold no mutator for any heap. Validate every
  recorded recursive depth in a first pass and treat a nonzero depth as a
  programmer-contract panic before changing the registry. Only after complete
  validation may a second step clear all records.
- Clearing is inert: do not upgrade a weak heap, dereference a cached run,
  return or clear a lease, change typed-run pressure, or invoke a callback.
  Leases from a live heap become ordinary abandoned capacity inside its already
  committed chunk until C5 revokes them. Re-entry later creates a fresh record
  and cursor map.
- Ordinary host-thread termination continues to drop its complete TLS registry
  automatically. The explicit operation exists for long-lived worker threads
  which outlive one or more runtime heaps, not as a required shutdown step.

Verification for C2C.5d:

- instrumentation proves repeated and recursive entry into a known heap never
  iterates unrelated TLS records or reads their weak counts;
- a dead record remains present when another heap is entered, retains no heap
  payload or arena, and continues preventing reuse of its Arc identity;
- explicit release removes both live-heap and dead-heap inactive records and
  returns the exact count without heap access;
- attempting release under one or several active mutator depths panics before
  removing any record, after which those mutators can exit normally;
- releasing a live heap's cache and re-entering it creates fresh TLS state,
  claims a new word, and changes no collection-pressure state unless it must
  publish a new typed run;
- ordinary thread exit still drops the registry without an explicit call; and
- focused lifecycle tests, multi-heap nesting, Clippy, the exact unsafe audit,
  Miri, and available sanitizer checks pass before the mandatory post-C2C
  review begins.

#### C2C.5 Completion

C2C.5 completed on 2026-08-22:

- Every lease word is now initialized and accessed as `AtomicU64`. Compile-
  time assertions latch its size and the stronger bitmap alignment contract.
  Claimers scan lease words, race with compare-exchange, retain a claimed full
  allocation word as unavailable, and never claim the invalid suffix of the
  final lease word.
- Allocation pressure no longer participates in word leasing. One centralized
  heap-state operation publishes a run into the arena and class pool, then
  charges one saturating typed-run event. At this checkpoint the 128th
  publication latched the provisional request; C3B subsequently lowered the
  trigger to 7/8 of one chunk when requests became serviceable. Neither a
  failed publication nor
  any lease, slot, cursor, or TLS transition adds an event.
- Each scoped allocator and heap class entry use one frontier cell. It release-
  publishes a pointer to a separately boxed, heap-owned run record, so ordinary
  cursor refill claims directly without heap state. Exhaustion rechecks under
  the mutex, advances monotonically through already published records, and
  publishes a new run only when no later record exists.
- Mutator entry performs only the direct TLS registry lookup for its heap. Dead
  weak records remain inert until thread exit or explicit
  `Heap::release_current_thread_caches`; explicit release validates all depths
  before clearing anything and never upgrades a heap, returns a lease, changes
  pressure, or invokes a callback.
- Focused verification covers concurrent hierarchical claims, full and partial
  lease words, lock-free refill, one-run publication under racing allocators,
  monotonic frontier advancement, publication failure, pressure thresholds,
  dead and live TLS records, and panic-atomic explicit release. The exact
  unsafe inventory records the new atomic raw-storage and stable-frontier
  boundary.
- The collector check passes with 72 unit tests, two Loom models, and six
  compile-fail doctests. Full leak-checking Miri passes all 72 unit tests, and
  both AddressSanitizer and ThreadSanitizer pass the collector library suite.
  Sanitizers deliberately exclude the separate Loom scaffold because even an
  empty Loom model retains one 256-byte tool-owned allocation under
  LeakSanitizer; the stable collector check continues to run both Loom models.

### C2C.6 Forced Exhausted-Frontier Verification

C2C.6 is a verification-only follow-up from the mandatory post-C2C review. It
does not change allocator semantics or introduce another post-phase review
gate. Complete it before C3A.1 so a frontier race cannot be confused with the
new mutator-admission coordinator.

- Add a deterministic test hook after `claim_allocation_cursor` observes an
  exhausted atomic frontier and before it acquires heap state for the
  synchronized recheck.
- Pre-exhaust one class's current run, release several claimers together at
  that hook, and force every participant to enter the synchronized slow path
  from the same stale frontier observation.
- Prove exactly one contender advances to or publishes the next run. Every
  loser must recheck under heap state, observe the winner's frontier, and claim
  a distinct allocation word from it.
- Latch instrumentation showing that an already published successor produces
  no pressure event, a newly published successor produces exactly one event,
  and no contender rescans the exhausted prefix.
- Keep the hook test-only and outside the production fast path. An abstract
  Loom refinement is optional; the required fixture exercises the production
  atomic frontier pointer, heap mutex, run pool, and pressure counter.
- Correct the existing lease-word comment and C2C safety proof: initial run
  topology is published either by the class frontier's Release store paired
  with its Acquire load, or by the heap mutex on the synchronized path. The
  lease-word Acquire is atomic word-ownership machinery at this stage; it does
  not pair with publication through a different atomic object.

Verification for C2C.6 runs the focused forced schedule repeatedly, followed
by the collector check, exact unsafe inventory, Miri, and available sanitizer
checks. Passing this checkpoint resolves GC2C-004 directly; it does not trigger
another mandatory C2 review.

#### C2C.6 Completion

C2C.6 completed on 2026-08-22:

- A test-only barrier pauses every participating claimant after its atomic
  frontier miss and before heap-state acquisition. Two production-path
  fixtures force eight claimers through the same stale observation, once with
  a prepublished successor and once requiring a new run.
- Both fixtures return eight distinct allocation words from one successor.
  Instrumentation proves exactly one frontier advance attempt, seven
  successful locked rechecks of the winner's frontier, no exhausted-prefix
  rescan, no pressure change for prepublished activation, and exactly one
  pressure event for new-run publication.
- The lease-word comment and safety ledger now distinguish initial run
  publication through the frontier Release/Acquire pair or heap mutex from
  C5's future same-atomic Release-reset/Acquire-claim edge. The test hook and
  counters are absent from production builds and add no unsafe operation.
- The stable collector check passes with 74 unit tests, two Loom models, and
  six compile-fail doctests. Full leak-checking Miri and both AddressSanitizer
  and ThreadSanitizer pass all 74 collector unit tests. Workspace formatting,
  Clippy with warnings denied, and the complete workspace test suite pass.

## Phase C3 — Regional Mutators and Stop-the-World Handshake

Execute C3 as the following checkpoints over the `ThreadHeapState` established
by C2C:

- **C3A.1 — coordinator and topology boundary.** Add the ordinary/exclusive
  phase representation and active-mutator accounting to the existing
  heap-state `Mutex`, plus one sibling `Condvar`, without collector election.
  Move allocation-class discovery from `Heap` to `Mutator`. This checkpoint
  originally left the discovered handle independently reusable; C4D later
  narrows it to the discovering mutator's borrow without changing the topology
  admission rule. Have `Mutator` borrow the heap's existing shared owner rather
  than cloning it per admitted region. Force discovery against a synthetic
  exclusive phase before building collection on this invariant.
- **C3A.2 — ordinary admission integration.** Split entry into
  prepare/admit/activate, integrate heap-qualified TLS activation, same-heap
  recursive entry/exit, panic-safe rollback and outer quiescence publication,
  and cache activation without collector election.
- **C3A.3 — admission proof.** Add release/acquire visibility tests,
  deterministic request/entry and unwind schedules, and the first real Loom
  model for the coordinator state. Record that completed mutator work becomes
  visible to the exclusive collector through the mutex-protected admission
  transition, not through an unrelated lease-word load.
- **C3B — single-heap STW.** Add collection request/coalescing, exclusive-
  collection commitment, collector election, active-count drain, exclusive
  phase entry, release back to ordinary admission for one heap, and replace
  C2C.5b's full-chunk provisional pressure threshold with the initial
  serviced-collection threshold.
- **C3C — cross-heap admission.** Add several simultaneously active
  heap-qualified TLS entries and the dependent-admission exception for queued
  collections. Force A-then-B/B-then-A schedules and the already-exclusive
  target case.
- **C3D — finalizer handoff and recovery.** Add the exclusive-to-finalizer-
  mutator transition, follow-up pressure, committed-collection priority around
  finalization, panic unwinding, waiter teardown, and the complete coordination
  audit required by later sweep/finalization phases.

Do not begin a later checkpoint until the preceding state machine and its
forced-order tests pass independently.

### C3A Completion

C3A completed on 2026-08-22:

- `HeapInner` now owns a mutator coordinator under the existing heap-state
  mutex and one sibling condition variable. `Ordinary`, `ExclusivePending`,
  and `Exclusive` distinguish open admission, committed drain, and completed
  exclusion. C3A exposes exclusion only through test-only synthetic admission;
  production request, election, and collection remain C3B work.
- Allocation-class discovery moved from `Heap` to `Mutator`. Canonical metadata
  and run geometry remain cold preparation, while every possible heap-local
  class-table publication now occurs within an admitted mutator region. The
  checkpoint returned an independently reusable, heap-retaining
  `AllocationClass<T>`; C4D deliberately narrows that result to an allocator
  capability borrowed from the discovering mutator while retaining class-table
  publication under the same admission.
- Thread-local entry is explicitly prepare, admit, then activate. Preparation
  changes no depth, an outer admission creates one RAII coordinator obligation,
  and activation refreshes the allocation-lease epoch before incrementing
  depth. Panic between admission and activation retires the obligation while
  leaving the cache inactive. Entry destruction makes the cache quiescent
  before retiring the outer obligation.
- Recursive same-heap entry reuses its outer admission, including while
  exclusion is pending. Different heaps retain independent TLS records,
  recursive depths, caches, and active counts. C3C still owns the special
  dependent cross-heap rule once production collections can be pending.
- Forced native tests cover discovery blocked by exclusion, retained handles,
  inactive preparation, pre-activation panic rollback, recursive discovery
  during drain, independent nested heaps, pending-exclusive priority, unwind
  restoration, and mutex-mediated visibility of prior mutator work. Two new
  coordinator Loom models cover exit-to-exclusive visibility and denial of a
  fresh outer entrant after exclusive commitment.
- The stable collector check passes with 82 unit tests, four Loom models, and
  six compile-fail doctests. Full leak-checking Miri and both AddressSanitizer
  and ThreadSanitizer pass all 82 collector unit tests. The exact unsafe
  inventory is unchanged by the safe coordinator and admission transition.
  Workspace formatting, Clippy with warnings denied, and the complete workspace
  test suite also pass.

### C3B–C3D Completion

C3B through C3D completed on 2026-08-22:

- `Heap::request_collection` now records an idempotent nonblocking request,
  while `Heap::collect_full` either joins the collection whose exclusion is
  not yet fixed or requests the following epoch. The latter rejects a
  same-thread active mutator before changing coordinator state. Exactly one
  requester elects each epoch; others wait on the condition variable and
  observe its completed report.
- Commitment publishes `ExclusivePending`, denies ordinary outer admission,
  drains the active count, and then publishes `Exclusive`. C3's collection
  body is intentionally synthetic: it proves stop-the-world coordination but
  performs no root scan, trace, sweep, lease revocation, or reclamation.
- The automatic typed-run threshold is now 7/8 of one 128-run chunk, or 112
  publications. Servicing the synthetic collection consumes the coordinator
  obligation but preserves C2C's publication count and pressure latch until a
  later successful C6 sweep has a sound rearming event.
- Heap-qualified TLS classifies a different-heap outer entry as dependent when
  this thread already holds another heap. Dependent entry may pass a committed
  `ExclusivePending` collector, but not `Exclusive`. A nested exit records a
  weak TLS-local service obligation; the thread's last ordinary heap exit
  services those deferred requests without waiting while it still holds an
  outer heap. This includes caught nested unwinds.
- After synthetic exclusive work, the collector prepares an inert TLS entry,
  then changes `Exclusive` directly to `Finalizing` while installing one
  collector-owned mutator obligation under the heap mutex. There is no
  authority gap. Ordinary workers may enter during finalization until a
  follow-up request commits the next collection.
- A request made during `Exclusive` or `Finalizing` belongs to the following
  epoch. Completing finalization atomically completes the current report and,
  when needed, publishes the next `ExclusivePending` epoch before releasing
  the mutex. Finalizer and exclusive-work panics retire active obligations,
  restore ordinary admission, and relatch the interrupted request.
- Forced native schedules cover explicit and automatic requests, same-thread
  rejection, waiter coalescing, reciprocal A-then-B/B-then-A dependent entry,
  already-exclusive targets, deferred nested service, the finalizer handoff,
  concurrent finalization entry, follow-up priority, and both exclusive and
  finalizer unwind. Loom additionally models reciprocal pending admission and
  the no-gap exclusive-to-finalizer transition.
- The stable collector check passes with 100 unit tests, six Loom models, and
  six compile-fail doctests. Full leak-checking Miri and both AddressSanitizer
  and ThreadSanitizer pass all 100 collector unit tests. The exact unsafe
  inventory is unchanged by C3's safe coordination work. Workspace formatting,
  Clippy with warnings denied, and the complete workspace test suite pass.

### Phase C3E — Entry-Serviced Collection Simplification

C3B through C3D deliberately proved the harder queued-drain protocol. Before
C4 adds roots to the stopped-world boundary, replace that protocol with the
simpler policy selected after review: an asynchronous request is a coalesced
heuristic hint, and collection begins only when an outer mutator entry finds
the heap already idle. Outermost exit publishes quiescence and wakes explicit
waiters, but does not itself perform collection or scan thread-local records
for work.

Execute the revision in three independently verified checkpoints:

- **C3E.1 — direct idle-entry election and handoff.** An outer entry which
  observes `Ordinary`, zero active mutators, and a latched request atomically
  becomes the collector. It enters `Exclusive` directly, without first
  publishing a writer-pending phase or denying admission while existing
  mutators drain. After exclusive work and finalization complete, that same
  thread atomically returns the coordinator to `Ordinary`, transfers its
  collector-owned finalizer obligation into the ordinary outer-mutator
  obligation for the entry it originally requested, and continues. The active
  count does not pass through zero and the thread does not release the
  coordinator and retry admission. A collector elected by `collect_full` has
  no mutator continuation and instead returns to `Ordinary` with no obligation
  of its own after finalization.
- **C3E.2 — cache and request retirement.** Before that directly elected
  entrant runs exclusive work, remove that thread's inactive,
  heap-local TLS cache, including every class-to-allocation-word cursor. Its
  collector-owned finalizer mutator therefore starts from a fresh cache, which
  can then pass directly to the requested ordinary entry. This is sufficient
  for the collector thread; C5's heap-wide lease epoch invalidates retained
  cursors belonging to every other thread. A successful collection clears the
  coordinator request and acknowledges the provisional typed-run pressure
  state. Requests observed before the completion transition are deliberately
  coalesced into the collection which just ran; a request serialized after
  completion remains latched for a later idle entry. Until C6 can rearm from
  actual survivor occupancy, C3 uses a documented provisional pressure reset
  rather than immediately repeating collection because the old publication
  threshold remains crossed.
- **C3E.3 — retire queued-drain machinery and re-audit.** Remove
  `ExclusivePending`, ordinary/dependent admission distinctions, TLS deferred
  collection-service records, last-thread exit scans, and follow-up-epoch
  chaining. Outermost exit only makes its cache inactive, decrements the active
  count, and notifies waiters. Preserve the no-gap
  `Exclusive`-to-`Finalizing` handoff and collector-owned finalizer mutator,
  but treat requests made during either phase as hints coalesced into the
  current collection rather than as an automatically committed second pass.

`Heap::collect_full` remains a stronger synchronous maintenance boundary. It
is invalid whenever the calling thread holds a mutator for *any* heap, because
the caller may otherwise participate in a cross-heap wait cycle. An eligible
caller records or joins the target collection and waits opportunistically for
an idle target without blocking new entrants merely because a writer is
queued. If another entrant or collector completes the requested collection,
the caller joins that epoch and returns its report; it does not require an
immediate follow-up collection. This operation may wait for active target
mutators to leave, while `request_collection` never waits and may remain
latched indefinitely on an unused heap. Dropping such a heap instead of
performing a final unused collection is desirable.

Collection or finalization panic does not count as successful completion: it
restores ordinary admission without directly admitting the interrupted
entrant, relatches the request, and leaves pressure eligible for a later
attempt. The direct ordinary handoff occurs only after the entire successful
collection/finalization lifecycle is complete.

Deterministic verification must force:

1. a request made while a mutator is active surviving outer exit, with no
   collection until the next outer entry;
2. several simultaneous outer entrants after one request electing exactly one
   collector, then all entering normally after that collection;
3. the collecting entrant continuing under one direct ordinary obligation,
   without a retry window in which another collector can intervene;
4. deletion of the collecting thread's inactive heap-local cursor cache before
   exclusive work, followed by fresh finalizer allocation and direct ordinary
   allocation from the replacement cache;
5. a request during `Exclusive` or `Finalizing` being coalesced, while a
   barrier-forced request after successful completion remains pending;
6. outer exit performing no collection and no scan or service of requests for
   other heaps;
7. `collect_full` rejecting a caller active in the target heap or any other
   heap before changing request or epoch state;
8. `collect_full` joining an active collection, and separately waiting for an
   active heap without preventing intervening mutator entries;
9. opposite A-then-B and B-then-A nested entry completing without a dependent
   admission category because an uncommitted request never blocks entry;
10. exclusive-work and finalizer panic restoring admission and relatching the
    interrupted request; and
11. provisional pressure acknowledgement preventing an immediate redundant
    collection on the collector's direct mutator handoff.

Replace the queued-drain Loom models with the smaller direct-election model.
It must prove that `Ordinary` plus zero active mutators changes atomically to
`Exclusive`, that no ordinary mutator overlaps `Exclusive`, that exactly one
entrant owns a requested collection, and that successful completion installs
the collecting entrant as an ordinary active mutator while clearing the
coalesced request. Keep the existing no-gap finalizer-handoff model, adjusted
for the absence of a pending phase.

### C3E Completion

C3E completed on 2026-08-22:

- The coordinator now has only `Ordinary`, `Exclusive`, and `Finalizing`.
  `ExclusivePending`, admission kinds, TLS deferred-service records, and
  exit-time heap scans are gone. Outermost exit only retires its obligation and
  wakes waiters.
- A requested outer entry elects collection only when it observes an idle heap
  under the coordinator mutex. It moves directly to `Exclusive`; otherwise the
  request remains a nonblocking hint and the entrant proceeds normally.
- Before exclusive work, the collector removes its inactive heap-local TLS
  record. Finalization creates a fresh record, and an entry-elected collector
  deactivates that record while preserving its coordinator obligation, then
  transfers the obligation directly into the original prepared entry. A
  `collect_full` collector drops the obligation instead.
- Successful completion clears requests received through finalization and
  resets the provisional run-publication pressure counter. A request serialized
  after completion remains latched. Collection or finalizer panic restores
  ordinary admission and relatches the interrupted request.
- `collect_full` now rejects a caller holding a mutator for any heap. Eligible
  synchronous callers do not reserve a pending writer: they opportunistically
  elect an idle heap or join the already-active collection epoch.
- Native forced schedules cover all eleven C3E cases above. Six Loom models
  cover allocation-word claims, visibility through mutator release, unique
  idle-entry election, reciprocal requested-heap nesting, and the no-gap
  finalizer-to-entry handoff. The stable collector gate passes with 105 unit
  tests, six Loom models, and six compile-fail doctests. Full leak-checking
  Miri, AddressSanitizer, and ThreadSanitizer pass all 105 collector tests.
  Workspace formatting, Clippy with warnings denied, and the complete root test
  suite also pass.

The C3 coordination narrative, `SAFETY.md`, and `VERIFY.md` now agree on the
entry-serviced state machine. C4 may begin.

#### Historical C3B–C3D Pressure Contract

The following pressure and admission text records the completed C3B–C3D
implementation which C3E replaced. It is retained as transition provenance;
it is not the policy for C4 and later phases.

- `Heap::request_collection` is the first explicit request API. It coalesces
  with the automatic request without changing the typed-run-publication count
  or allowance.
- Keep the provisional publication count and request latched through C3's
  synthetic collection and C5 marking. Neither phase has reclaimed or
  republished reusable storage, so neither has a sound event at which to reset
  the C2C history.
- Verify explicit requests below the automatic threshold, request coalescing,
  successful exclusive admission, and abandoned or panicking synthetic
  collection without treating any of those transitions as a typed-run
  publication.

#### Historical C3B–C3D Coordination and Admission Contract

- Implement outer `enter`/`exit` admission as an explicit state machine under
  the existing heap-state `Mutex`, with one sibling `Condvar`. Store the phase,
  active-mutator count, collection request/commit state, and later collector
  identity or epoch under that mutex. Waiters always loop and recheck their
  complete predicate after a wake.
- Represent an admitted outer mutator by one coordinator obligation. Admission
  increments the active count and releases the mutex before user or allocator
  work begins, so parallel mutators do not retain a lock or serialize through
  the coordinator. Outermost exit reacquires the mutex only long enough to
  retire that obligation and notify waiters; recursive same-heap entry does
  not add another active obligation.
- Make `Heap::request_collection` an idempotent nonblocking transition into the
  coordinator's coalesced request state. Calling it from an admitted mutator
  never attempts collection recursively and never waits for that same region
  to leave.
- Before automatic pressure can initiate a real collection, replace C2C.5b's
  provisional `RUNS_PER_CHUNK` trigger with
  `RUNS_PER_CHUNK * 7 / 8`. With the initial 128-run chunk this latches a
  request on the 112th successful typed-run publication, leaving 16 runs
  (1 MiB) of nominal allocation headroom before another 8 MiB chunk is needed.
  Keep the calculation integral and compile-time checked for the fixed run
  geometry. This is a trigger rather than a hard capacity limit: a mutator
  does not service the request until its outermost exit and may overshoot the
  headroom. Tests should latch the boundary at 111/112 publications and prove
  later publications remain coalesced.
- Treat every outermost mutator exit as a safepoint for servicing explicit and
  committed heuristic requests. Recursive same-heap exits merely decrement the
  thread-local depth. On outer exit, publish cache quiescence and decrement the
  heap's active count under the coordinator; if this was the final active
  mutator, elect that thread or wake one waiting requester according to the
  single-collector rule. Evaluate only cheap runtime-owned heuristic state at
  this boundary and invoke no user callback while holding coordinator state.
- Keep `Heap::collect_full` as a synchronous maintenance operation for callers
  outside that heap's mutator regions. Detect a same-thread active mutator from
  the heap-qualified TLS entry and return `CollectionError::ActiveMutator`
  rather than deadlocking. Other active threads may finish normally while the
  synchronous caller participates in the standard request/election protocol.
- Admission and run-publication slow paths deliberately share the existing
  `HeapState` mutex. Keep their fields and transitions separately documented.
  The mutex belongs to arena-chunk, typed-run-pool, class-discovery, and phase
  state; it is not held by a mutator's local allocation-word cursor or for the
  lifetime of a mutator. The collector sets its request under that mutex, waits
  on the condition variable—which releases the mutex—until the active count
  reaches zero, then atomically publishes `Collecting`. The phase grants
  exclusive allocation access after the mutex is released.
- Extend C2C's `ThreadHeapState` with collection admission. It already contains
  that heap's recursive mutator depth, allocation-lease epoch, and allocation-
  word cursor map. A thread may have several such states active concurrently;
  entering another runtime heap activates its independent state and admission
  count rather than replacing a singular current-mutator slot.
- Support recursive same-heap entry through that heap's thread-local depth
  without incrementing its active-mutator count again.
- Activate the persistent C2C thread cache at the outermost same-heap entry.
  Nested entry reuses that cache. Only outermost exit makes the cache quiescent,
  leaving reusable cursors retained; eviction and exit never return a word
  lease.
- Do not maintain a cross-thread active/parked flag for a cursor. Recursive
  depth is local to its owning thread. The collector need not inspect another
  thread's depth: acquiring exclusive mutator admission proves that every
  TLS cache is quiescent. It then revokes word leases by clearing heap-owned run
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
- Once a collection request is committed by an elected collector, it prevents
  ordinary new outer entries, then waits for every active mutator of that heap
  to exit. A merely recorded nonblocking request is committed by a synchronous
  requester or a safe outer-exit servicing boundary. There is one narrow dependent-
  admission exception: a thread already holding another heap's mutator may
  enter a target heap whose collector is requested or queued but has not yet
  acquired exclusive `Collecting` state. Under the target heap-state mutex,
  the dependent entry either increments its active count or observes that the
  collector is already exclusive and waits; there is no gap between those
  outcomes.
- Encode ordinary and dependent admission as explicit coordinator inputs. A
  committed collection denies an ordinary outer entry, permits the narrow
  dependent entry while the collector is only queued, and denies both once
  `Collecting` is authoritative. The heap-state mutex makes that classification
  and transition atomic; the condition variable only supplies wakeups and
  every waiter rechecks the full predicate.
- Give the collector a privileged collector-to-mutator handoff. After marking
  fixes the dead set, and before releasing exclusive mutator admission, the
  collector acquires one ordinary mutator lease for its own thread. With an
  active-count state machine it increments the active count and publishes
  `Finalizing` while still holding the heap-state mutex. There must be no
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
- Once a collection is committed, it has priority over new ordinary mutator
  admission because the heuristic has selected stop-the-world work as the
  runtime's next operation. A dependent cross-heap entry from an already active
  mutator bypasses only that pending commitment, not an active collector. This
  bounded exception prevents two threads holding A then B and B then A from
  deadlocking merely because collections are pending on both heaps. Tune the
  commitment heuristic rather than weakening this admission priority.
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
  Outermost exit retains its local allocation-word cursors, makes the cache
  quiescent by leaving its recursive region, and then decrements the active-
  mutator count. The admission release/acquire edge makes prior allocator
  metadata writes visible to the collector; it is not a Glam value-publication
  or transactional boundary. Exercise this ordering under Loom or
  deterministic barriers.
- Allocation thresholds request collection but do not synchronously collect
  from the middle of a mutator region.
- Allocation pressure during `Finalizing` records follow-up pressure. Before
  commitment it does not block finalizer allocation. If the heuristic commits,
  the follow-up collection may block new ordinary mutators immediately, but
  cannot become exclusive before the finalization queue drains and the held
  finalizer mutator is released.
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
7. commitment of the next collection blocking a new ordinary mutator;
8. a panicking mutator;
9. two heaps nested on one thread with independent recursive depths and caches;
10. two threads entering A then B and B then A while collectors are pending on
    both heaps, proving dependent admission breaks the wait cycle;
11. a cross-heap entry waiting for an already-exclusive collector without that
    collector reaching into the held heap; and
12. a request made inside a recursively entered mutator remaining pending
    through the inner exit and being serviced only at outer exit;
13. `collect_full` called from a same-thread active mutator returning
    `ActiveMutator` without partially committing a collection or waiting; and
14. heap drop while collection waiters exist.

Use Loom for the coordination state where feasible. Repeated stress is
supplementary, not proof.

### Post-C3E Review of C4 through C8

Reviewed on 2026-08-22 against the implemented C3E coordinator, typed-run
allocator, heap-qualified TLS caches, canonical object metadata, and trace
visitor.

The downstream structure remains sound, with these corrections:

- C4 can use the existing heap-state mutex for release-validating registration
  and a collection-time walk of the stable root registry. It does not need
  another root-registry mutex or a full strong-root snapshot. Root clone and
  drop remain lock-free apart from ordinary `Arc`/`Weak` ownership.
- C4 fixes one deliberately narrow collector representation. `Root<T>` is one
  `Arc<RootCell>` plus a zero-sized typed marker. The non-generic `RootCell`
  contains a weak heap identity and exactly one erased direct `Gc`; the heap
  registry contains thin `Weak<RootCell>` entries. The collector does not
  generalize this into arbitrary or inline registry payloads. Phase I2 may
  wrap a direct root to keep public scalars inline, without changing the
  collector registry or root-cell representation.
- C5 is mark-only. Revoking allocation-word leases, advancing the heap-wide
  lease epoch, resetting class frontiers, and publishing free allocation state
  belong to C6A immediately before reclaimed storage can become reusable.
  Marking alone neither changes allocation bits nor invalidates retained
  allocation cursors.
- C5 uses a conventional clear-before-mark bitmap. Under exclusive authority,
  clear every assigned run's contiguous mark range before tracing, then use
  `1` for every reachable slot. A failed attempt publishes no mark result and
  may leave arbitrary scratch bits behind; the next attempt's mandatory
  initial clear overwrites them. This intentionally trades a cache-local
  bitmap pass per collection for a much smaller correctness surface: there is
  no mark color, allocation-time mark write, touched-word journal, or failure-
  time bitmap pass. A successful result remains authoritative long enough for
  C6's eager dead-set classification and sweep.
- C6 must refer to C3E's direct finalizer-to-entry handoff, not the superseded
  C3D queued-drain protocol. Its previous four checkpoints were still too
  broad, so they are divided below at free-list, eager-sweep, finalizer,
  quarantine, pressure, and terminal-teardown boundaries.
- C7's `deferred_requests` metric is obsolete. Entry-elected collections,
  coalesced request observations, pending hints, synchronous joins, and
  collection latency describe the implemented coordinator.
- C8 must distinguish build-time geometry experiments from per-heap tuning.
  Arena-chunk and fixed-run sizes participate in pointer masking and layout
  constants, so the initial collector compares them through builds or private
  const-generic fixtures rather than runtime heap options. Thresholds and
  reporting policy may remain per heap.

The later integration plan remains intentionally provisional until Gate G1.
C4 fixes the collector's direct-root representation but does not preempt I2's
choice between using it directly and placing it behind a Glam-owned public
value wrapper. C6D still owns the last-value-domain-owner teardown decision.
No production integration phase is pulled into the isolated collector plan by
this review.

## Phase C4 — External Root Registry

Execute C4 as three independently verified checkpoints:

- **C4A — checked direct-root construction and access.** Add the release-build
  slot lookup needed to prove that a `Gc<T>` names an allocated slot with the
  canonical metadata in the mutator's heap. Because this is the first
  concurrent reader outside an allocation-word lease, represent allocation
  words as atomic single-writer/multi-reader state; publish initialized payloads
  with Release and validate rootability with Acquire. Build the cloneable
  `Root<T>` and its root cell on that proof. Represent `Root<T>` as an
  `Arc<RootCell>` plus a zero-sized typed marker; represent the non-generic cell
  as a `Weak<HeapInner>` plus one `ErasedGc`. A root is a liveness claim within
  a live heap, not an owner of that heap. `Root::get` therefore requires a
  matching live mutator, rejects a different heap before invoking the private
  unsafe `Gc` access gateway, and provides no self-entering `Root::with`
  operation. Root
  provenance compares the cell's weak heap identity with the mutator's heap;
  the surviving weak control block prevents address reuse from making a stale
  root appear to belong to a new heap. Latch the intended one-pointer
  `Root<T>` size with a compile-time assertion while preserving the selected
  variance and `Send`/`Sync` bounds through its zero-sized marker. Root
  construction is permitted only from a live `Gc<T>` during a mutator region,
  provisionally as `Mutator::root`. Keep the constructor crate-private until
  C4B makes registry publication part of its return invariant. C4's rootable
  state is simply `allocated`; C6 extends the same lookup to reject completed
  dead and finalization-batch identities.
- **C4B — weak registry publication and stable traversal.** Register one thin
  `Weak<RootCell>` before returning the first root and store the registry in
  `HeapState`, so validation and publication share its existing mutex and no
  second component lock is introduced. Once collection has stopped every
  mutator, root construction and registry publication cannot proceed; the
  collector may therefore walk that stable registry directly. For each entry,
  upgrade the weak cell only for the duration of reading and visiting its
  erased `Gc`, prune failed upgrades in place, and release the temporary strong
  reference before continuing. Do not build a `Vec<Arc<RootCell>>` snapshot.
  Construct the `Arc<RootCell>` and its weak candidate before taking heap
  state. The active mutator is the semantic exclusion barrier between
  validation and publication; using one state-lock acquisition for both is an
  implementation economy because current topology validation already requires
  that mutex, not an additional atomicity requirement.
- **C4C — concurrent lifetime and boundary audit.** Integrate the stable
  registry walk into the exclusive collection path even though C4 does not
  trace the seeds yet. Force clone, drop, per-entry upgrade, pruning, and
  heap-facade-drop orderings; specifically prove that releasing the collector's
  temporary upgrade as the last strong cell reference runs only passive cell
  destruction. Then update the safety ledger and run the complete collector
  verification matrix. This checkpoint is a lifetime/concurrency audit, not a
  marking implementation.

Cloning a root may use ordinary atomic ownership at this external boundary;
internal `Gc` copies remain free of that cost. Root destruction acquires no
allocator or registry lock. `RootCell` destruction is operationally passive:
it drops only a weak heap reference and erased pointer bits, never `T`, a user
callback, a lock-taking registry token, or a runtime-entry guard. The ownership
graph stays acyclic: authorized value-domain owners retain the heap, roots
retain their cells, cells refer weakly to the heap, and the heap registry
refers weakly to cells.

Concurrent clone/drop changes only the strong count of an already registered
cell. A successfully upgraded registry entry keeps the seed live through that
one visit; a failed upgrade proves no public root remained at that instant.
Dropping the final public root after its seed is visited may conservatively
preserve the referent for one collection, which is safe. Because creation
requires mutator admission, no otherwise unreachable bare pointer can become a
new root after exclusive collection begins. Cloning an existing root during
the pause changes no seed or registry membership.

Verification forces root construction and access, representation mismatch,
foreign-heap creation and access rejection, cloning, cross-thread sharing,
final drop, heap-facade drop with a surviving but unusable root, and registry
pruning on both sides of the per-entry upgrade boundary. Deterministic hooks
prove publication precedes return, a temporary upgrade safely survives the
last public drop through its visit, releasing the last upgraded cell is
passive, and a failed weak upgrade cannot race a new root into existence.

#### C4A Completion

C4A completed on 2026-08-22 with the following boundary:

- Allocation bitmap words are initialized as `AtomicU64`. Their one leased
  writer uses a Release store after payload initialization; root checks and
  heap-side readers use Acquire loads. Word leasing and the allocation hot
  path otherwise retain their existing shape.
- `Root<T>` is one `Arc<RootCell>` plus a zero-sized type marker, with its
  one-pointer size statically asserted. `RootCell` contains only
  `Weak<HeapInner>` and `ErasedGc`; dropping it cannot touch the payload, enter
  the runtime, invoke user code, or retain the heap.
- Crate-private `Mutator::root` validates exact indexed heap membership,
  canonical type metadata, and current allocation before constructing the
  cell. Public `Root::get` performs an all-build heap-identity check and binds
  its borrow to a matching live mutator. The retained weak control block makes
  that pointer identity immune to heap-address reuse while the root exists.
- Focused tests cover the one-word and `Send + Sync` contracts, later-region
  and cross-thread access, foreign construction and access, representation and
  unallocated-slot rejection, observation while the leased word advances, and
  terminal heap destruction with an escaped root.
- Root construction deliberately remains crate-private and is currently used
  only by unit tests. C4B must publish the cell into the heap's weak registry
  before exposing that constructor; C4A adds neither root scanning nor
  reclamation.

#### C4B Completion

C4B completed on 2026-08-22 with the following boundary:

- `HeapState` owns one `Vec<Weak<RootCell>>`. Public `Mutator::root` constructs
  a candidate cell, then validates its heap, canonical representation, and
  allocation state and publishes exactly one weak entry under the existing
  heap-state mutex before returning the root. Cloning a root publishes
  nothing.
- Invalid root construction releases the heap mutex before raising its
  contract panic, leaving the heap usable and the registry unchanged. A
  mutator blocked behind exclusive collection authority cannot publish a root
  until ordinary admission resumes.
- Collector-private traversal requires exclusive authority and zero active
  outer mutators. It uses `Vec::retain` to upgrade and visit each live cell in
  registration order, release that temporary strong reference before the next
  entry, and compact dead weak entries in place without constructing a strong
  snapshot.
- Focused tests cover per-cell rather than per-clone publication, failed
  validation without partial publication or mutex poisoning, publication
  exclusion during a collection pause, ordered visitation, and dead-entry
  pruning. C4B does not yet invoke the traversal from a production collection;
  C4C owns that integration and the concurrent lifetime audit.

#### C4C Completion

C4C completed on 2026-08-23 with the following boundary:

- Every elected collection now walks the weak registry during its exclusive
  phase. C4 supplies a no-op seed receiver, so the walk prunes dead cells but
  does not mark or reclaim payloads; C5 replaces that receiver with exact
  marking.
- A forced per-entry schedule pauses after a successful weak upgrade, drops
  the final public root on another thread, and then proves traversal reaches
  the next entry while retaining the heap-state mutex. The temporary collector
  `Arc` keeps the first seed valid through its visit, is explicitly dropped
  before the next entry, and runs only passive `RootCell` destruction.
- A cell which was live at upgrade is conservatively retained in the weak
  vector for that collection even if its final public root is dropped during
  the visit. The following walk observes the failed upgrade and prunes it in
  place. Its managed payload remains allocated until heap teardown.
- Production-path tests prove an elected collection prunes expired roots.
  Another forced schedule proves a failed upgrade cannot race replacement
  publication: root construction stays blocked until exclusive authority ends,
  then publishes a distinct new cell normally.
- Existing cross-thread clone/access and escaped-root teardown tests complete
  the ownership audit. C4 adds neither trace dispatch nor reclamation.

### C4D — Mutator-Scoped Allocator Capability

Complete this ownership correction before C5 introduces collector traversal:

- Replace the public reusable `AllocationClass<T>` with
  `Allocator<'mutator, T>`, constructed only by borrowing an admitted
  `&'mutator Mutator`. Give the allocator real borrowed access to the mutator's
  heap/cache capability rather than manufacturing an unconstrained lifetime
  with `PhantomData` alone.
- Move typed allocation to `Allocator::alloc(T)`. Keep process-wide metadata,
  dense `AllocationClassId`, class entries, run pools, and frontier records
  heap-owned implementation details.
- Remove the allocator's strong `Arc<HeapInner>`. A scoped allocator may clone
  the stable frontier cell for the duration of its borrow, but that cell has no
  back-reference to the heap. Forgetting such a value may leak only an inert
  frontier cell, never the heap or an exercisable post-region capability.
- Preserve `Heap::with_mutator`'s higher-ranked closure boundary. Safe Rust
  must reject returning an allocator from the closure, placing one in a
  `'static` container, or sending it to another thread. Allocated `Gc<T>` and
  explicitly published `Root<T>` remain the intended escaping products.
- Retain foreign-heap checks at private cached-identity and raw-state
  boundaries, but make construction of a public foreign class/allocator pair
  unrepresentable. Do not pay a runtime-domain check per allocation merely to
  compensate for an escapable handle the API no longer exposes.
- If repeated discovery across short mutator regions later measures poorly,
  cache the canonical-metadata-to-dense-class/frontier result in the existing
  per-thread/per-heap state. Such cache records retain only weak heap identity
  plus internal class data, are cleared by the existing epoch/release rules as
  appropriate, and never become user-visible capabilities. Do not add this
  cache speculatively in C4D.
- Migrate production call sites and fixtures which currently carry a class
  across `with_mutator` regions. Add compile-fail tests for closure escape and
  cross-thread transfer, plus runtime tests proving repeated scoped discovery
  still selects one heap-owned class and retains the lock-free frontier fast
  path.

C4D settles the class-handle portion of terminal ownership: allocation
capabilities are not authorized heap owners and none survive a drained mutator
set. C6D still owns the terminal-finalization protocol itself.

#### C4D completion

Completed on 2026-08-23:

- The public reusable `AllocationClass<T>` and `Mutator::alloc` surface is
  removed. `Mutator::allocator` now returns `Allocator<'mutator, T>` with real
  borrows of the admitted mutator's heap and thread cache, and allocation is
  exclusively `Allocator::alloc(T)`.
- Durable class entries, run pools, and atomic frontier state remain heap
  owned. The collector-private typed class identity carries only a non-owning
  heap pointer, canonical metadata/class identity, and a stable frontier cell;
  it is neither exported nor an additional heap owner.
- `Heap::with_mutator`'s higher-ranked closure rejects allocator escape, and a
  separate compile-fail fixture rejects scoped-thread transfer. `Gc<T>` and
  explicitly registered `Root<T>` remain the escaping value forms.
- Public foreign allocator construction is unrepresentable. Private raw-state
  boundaries retain heap-provenance checks, while the hot allocation path uses
  constructive provenance plus a debug assertion instead of a release-build
  domain comparison per object.
- Fixtures now reacquire scoped allocators while reusing the same heap-owned
  dense class and lock-free frontier. Terminal teardown no longer waits on an
  escaped class handle, and a forced `mem::forget` fixture proves that leaking
  an inert scoped allocator does not retain its heap.
- Removing heap ownership from the class identity exposed one stale unsafe
  proof around the raw frontier pointer. Frontier reads now require a live
  `HeapInner` borrow, so admitted mutator authority—not handle ownership—keeps
  the heap-owned run record alive during dereference.
- No cross-region class cache was added. Repeated metadata/class lookup remains
  a cold operation to profile before introducing weak per-thread/per-heap
  caching.

The authoritative crate check passes formatting, Clippy with warnings denied,
the unsafe inventory, 121 unit tests, 6 Loom models, and 8 compile-fail/doc
tests. C5A.0a is now the next implementation checkpoint.

### Post-C4 Review of C5 through C8

Reviewed on 2026-08-23 against the completed C4 root registry, the current
single `HeapState` mutex, the C3E coordinator, stable allocation-class
frontiers, and allocation-word leasing. C4D was added afterward to remove
escaped class ownership before these downstream phases begin.

The implemented C4 boundary is sound. Root construction validates and
registers under mutator exclusion, roots refer weakly to the heap, and the
exclusive registry walk may conservatively retain a cell for one collection.
No C4 ownership or ordering repair is required before marking. The following
downstream corrections are required:

- **Separate coordination from managed heap data before tracing.** The current
  `HeapState` mutex contains the coordinator together with arena, class,
  pressure, and root data. Holding it through a full trace would prevent
  `request_collection` from remaining nonblocking, while reacquiring it for
  every edge would introduce exactly the pointer-scale locking this collector
  avoids. C5A.0a-b therefore split a coordinator mutex/condition variable from
  one managed-data mutex, move the coalesced request bit to a sibling atomic,
  and audit the resulting sequencing. Never hold the two component mutexes
  together. The coordinator mutex protects phase, active-mutator count,
  collector election, active/completed epochs, and condition-variable wait
  predicates; it is never held through trace, sweep, finalization, or a
  mutator closure.
  Run-pressure publication updates managed data and sets the atomic request
  before releasing the data lock. Successful completion publishes its final
  pressure baseline and clears that same atomic while holding the data lock.
  A later pressure publication must acquire the data lock afterward and
  therefore relatches the request; an unrelated external request linearizes
  immediately before or after the atomic clear. No pressure revision is
  required. This is an implementation lock split, not a new semantic phase.
  Merely setting the request bit does not change any condition-variable wait
  predicate, so asynchronous `request_collection` performs no `notify_all`.
  Notifications remain reserved for changes to coordinator phase, active
  obligations, and completed epochs.
- **Clear marks before tracing rather than coloring allocations.** After
  exclusive admission drains every mutator, C5 clears the contiguous mark
  range of every assigned run before it seeds or traces the graph. Reachable
  slots are then represented by set bits. Allocation remains entirely
  independent of mark state: a mutator captures no collection color and the
  allocation-word owner performs no mark-bit write before publishing an
  allocation. This keeps the frequent allocation path simple and confines all
  mark interpretation to exclusive collection and its later sweep consumer.
- **Keep root seeding bounded and non-fallible under the data lock.** C5 first
  reserves a `Vec<TraceWork>` using the registry length, then retains weak
  entries and upgrades them while exclusive authority keeps root publication
  stopped. Each live registry entry increments the root count, but its exact
  slot is marked before enqueueing and only a newly marked allocation is
  pushed. That same vector is the LIFO object worklist, so duplicate root cells
  never create duplicate work. Trace dispatch and further worklist growth
  occur only after the registry walk. This preserves C4's no-strong-snapshot
  intent: temporary upgrades are released within the walk, and copied managed
  pointers are safe because no sweep occurs during the mark attempt.
- **Validate before the first unsafe trace dereference.** Same-heap, exact
  slot, allocated-state, and canonical-metadata checks are part of C5B.2's
  basic traversal, not a later hardening pass. C5C.1-2 separately prove
  recovery from those invariant panics and from visitor/worklist panics.
  Because marking may hold the managed-data mutex, its unwind guard must
  recover that mutex after poison, invalidate and discard the attempt-local
  result, restore coordinator state, and then resume the original panic.
  Partial bitmap contents remain unpublished scratch until the next attempt
  clears them.
- **Do not duplicate run occupancy outside the bitmaps.** C5 keeps only scalar
  attempt counters such as roots, newly marked slots, traced objects, and
  conservatively retained quarantine slots. It allocates no map, vector, or
  record per run. After a successful mark, C6 enumerates the authoritative
  assigned runs and derives zero, partial, and full occupancy directly from
  their compact allocation and mark words. A failed attempt publishes neither
  aggregate counters nor pressure or reclamation state. One currently spare
  `u32` in `RunHeader` remains available for a measured future live-slot count,
  reset alongside marks, but the baseline does not consume it.
- **Reclamation must invalidate TLS cursors before changing topology.** C6A.2a
  revokes allocation-word leases, withdraws ordinary class-frontier selection,
  and advances the heap allocation-lease epoch while every mutator is stopped.
  No run record, allocation bit, or reclaimed slot changes before that
  transition. Every later entrant discards its retained cursor map before it
  could observe run retirement, eager sweep, or reuse.
- **Retiring a run must retire every lock-free selector first.** A class owns
  stable boxed run records and publishes a raw frontier pointer. C4D ensures
  every allocator which can read that pointer is scoped to an admitted
  mutator; no escaped public class handle remains. After C6A.2a has drained
  mutators and invalidated TLS cursors, C6A.2b must clear or replace the
  heap-owned frontier, remove the run from the class pool, and repair any
  shifted frontier index before the record can be destroyed or its run can be
  retyped. Header reset and free-list publication belong to C6A.2c.
- **Sweep no-drop allocation words eagerly.** After C6A.2a invalidates every
  retained cursor and lease, the collector is the sole allocation-bitmap
  writer under `Exclusive`. Retire wholly dead no-drop runs, then intersect
  each retained no-drop allocation word with its successful mark word. This
  enumerates compact side bitmaps rather than payloads and completes every
  no-drop reclamation obligation before ordinary admission reopens. Rebuild
  lease availability and class frontiers only afterward, excluding words and
  whole runs reserved for drop finalization. No sweep epoch, unswept bitmap,
  first-claim branch, or retained-mark dependency enters the allocator.
- **Finalization reserves partial runs by word and wholly dead runs in full.**
  Ordinary mutators may allocate while the collector is in `Finalizing`. For a
  partially live run, the collector therefore removes every allocation word
  containing a finalization-batch slot from ordinary lease/frontier selection
  before reopening admission; unrelated words in that run remain usable. A
  wholly dead drop-bearing run is instead detached from allocator selection in
  full while still under `Exclusive`, because no live allocation benefits from
  retaining a partial allocator view. The finalizer owns the reserved storage
  until its corresponding slots are cleared or quarantined. A terminal partial
  word is rebuilt and republished immediately. A wholly dead run whose complete
  run batch succeeds is retired and published to the free-run pool immediately,
  without waiting for another collection; quarantine instead restores the run
  to its original class with the damaged slots retained as allocated. This
  preserves the allocation bitmap's one-writer rule without making every
  drop-bearing run unavailable or preventing reclaimed runs from being reused
  across collection cycles.
- **Quarantine and terminal teardown must agree.** Terminal destruction must
  consult sparse quarantine so a destructor which already panicked is never
  invoked twice. C4D settles allocation capability ownership before C6:
  mutator-scoped allocators are not heap owners and cannot survive the admitted
  region which permits their frontier access. C6D.1 therefore decides only the
  remaining runtime/value-domain owner drain and finalizer protocol.
- **C7 and C8 are verification/tuning phases, not new semantics.** Split
  collection/finalizer metrics from allocator/cache metrics and audit their
  consistency separately. Stabilize the report accumulated by C5 and C6 in
  C8A; build a measurement harness before selecting geometry experiments or
  workloads. Gate G1 remains owned by C6D.3, so production integration is
  still blocked even though C4 now supplies its direct-root primitive.

The remaining deliberate decisions are localized: C6A.3b chooses when to
physically clear a consumed successful mark bitmap, C6B.1 chooses whether an
empty batch skips `Finalizing`, and C6D.1 settles the remaining terminal owner
drain. C4D has already settled allocation-capability ownership. None blocks
starting C5A.0a after C4D completes.

The review reran the crate's authoritative `scripts/check.sh`: formatting,
Clippy with warnings denied, unsafe-inventory checks, 120 unit tests, 6 Loom
models, and 6 compile-fail/doc tests passed. The C4 tests directly force root
publication before return, failed validation without partial publication,
last-public-root drop after registry upgrade, pruning on the next walk, and
root publication blocked across exclusive collection. No new C4 verification
gap was found.

## Phase C5 — Exact Full Marking

Execute C5 as the following independently verified checkpoints:

- **C5A.0a — mechanical coordinator/data lock split.** Move
  `MutatorCoordinator` and its condition variable behind one coordination
  mutex. Keep arena, class, allocation pressure, root registry, and future
  mark/sweep state behind one managed-data mutex. Adapt admission, request,
  collection, allocation, root, and deterministic-test call sites while
  preserving behavior. Establish the structural rule that the two component
  mutexes are never held together.
- **C5A.0b — atomic request and data-side acknowledgement.** Move the coalesced
  request to a heap-sibling `AtomicBool`. Asynchronous requests only set it;
  they acquire neither mutex and send no condition-variable notification.
  Pressure publication sets it before releasing managed data. Successful
  completion publishes the final pressure baseline and clears the bit under
  that same data lock; failure relatches it before restoring ordinary
  coordinator state. Keep synchronous `collect_full` under the coordinator
  protocol so it joins an active epoch or requests and elects the next one.
  Add no pressure revision or duplicated coordinator request flag.
- **C5A.0c — split-state forced-order audit.** Prove `request_collection`
  remains nonblocking while exclusive work holds managed data, and replay
  every entry, request, finalizer, root-publication, and pressure schedule
  which crosses the split. Verify pressure publications on both sides of the
  data-side acknowledgement, external requests racing its atomic clear,
  synchronous join during a held data lock, and absence of a request-only
  notification path. This latches the sequencing argument from the post-C4
  review before mark state is added.
- **C5A.1 — clear-before-mark bitmap operations.** Add collector-only
  operations to clear every assigned run's contiguous mark range and to set or
  test an individual slot mark. Clear the ordinary `u64` mark-word slice with
  a contiguous bulk fill so optimized builds may lower the operation to their
  normal memset path. Run the initial clear only after exclusive admission has
  drained every mutator and before root seeding. Allocation does not read or
  write mark state. Prove complete clearing across zero, one, and many assigned
  runs, marking at bitmap-word boundaries, duplicate marking, and absence of
  any mark operation on the mutator allocation path.
- **C5A.2 — checked collector lookup.** Add all-build collector lookup from a
  managed address to its owning chunk, exact run and slot, allocation state,
  and canonical metadata. Keep no run-keyed attempt state; the slot mark is the
  only per-allocation reachability record.
- **C5A.3 — failed mark-attempt invalidation.** Add the non-recursive attempt
  guard. On failure, recover the managed-data mutex if necessary, discard every
  attempt-local aggregate counter and work item, restore ordinary coordinator
  state, and resume the original panic. Do not scan or clear the partially
  written mark bitmap: it is unpublished scratch, and the next attempt begins
  with C5A.1's mandatory clear. Prove with synthetic mark writes and injected
  panics that zero, one, and many partially marked runs publish no result,
  leave allocation and root state unchanged, and are completely overwritten
  by a clean retry. This checkpoint does not dispatch user `Trace`
  implementations yet.
- **C5B.1 — stable root seeding.** Under exclusive authority, reserve seed
  capacity outside the data lock from a previously observed stable registry
  length, then reacquire managed data and retain weak entries. Count every
  successfully upgraded root-registry entry, but validate and mark its slot
  before enqueueing its `ErasedGc`; only the unmarked-to-marked transition
  pushes. Multiple roots for one allocation therefore contribute separately
  to `root_count` while that allocation enters the worklist at most once.
  Release every temporary strong root-cell reference during that walk.
  Perform no trace dispatch or fallible growth during the registry walk
  itself. C5B.2 reuses this vector directly and may grow it while tracing under
  managed data; C5C's unwind path treats that allocation as fallible attempt
  work.
- **C5B.2 — checked non-recursive graph marking.** Drain the explicit worklist
  through canonical metadata and the existing edge visitor. A popped item is
  already marked as discovered and is traced exactly once; do not interpret
  its set bit as a reason to skip tracing. Every reported edge passes C5A.2's
  same-heap, exact-slot, allocated, and canonical-metadata check, then marks
  before enqueueing. Duplicate roots, repeated edges, cycles, and diamonds
  therefore terminate at discovery and never add duplicate worklist entries.
  Successful discovery stores the erased pointer together with its recovered
  canonical metadata in a private `TraceWork`, preserving that proof until
  drain. Exclusive authority keeps the allocation and metadata association
  stable, and drain holds managed data during dispatch, so it calls the
  retained metadata directly without a redundant second lookup. Cover cycles,
  diamonds, deep chains, wide graphs, and shared logical collection spines
  without changing allocation, lease, frontier, or pressure state.
- **C5C.1 — trace and worklist panic recovery.** Force visitor and worklist
  panics after zero, one, and many reported edges, plus injected worklist
  publication panics after zero, one, and many completed enqueues. Make actual
  edge-driven capacity growth use `try_reserve` before `Vec::push`, so capacity
  exhaustion is an ordinary attempt panic rather than an implicit infallible
  allocation assumption. Invalidate the partial marks, discard proof-carrying
  work items and aggregate counters, recover managed data and coordinator
  phase, preserve or coalesce the request hint according to the existing
  coordinator contract, then prove ordinary mutation and a clean retry.
  Invalidation is logical: it publishes no result and performs no bitmap
  clear.
- **C5C.2 — invalid-edge recovery and retry.** Force live foreign-heap, stale
  foreign, non-slot, and unallocated reported edges before unsafe dereference.
  Each invariant violation panics, publishes no mark result, and leaves both
  heaps usable. A still-reachable invalid edge must panic again on retry.
  Canonical metadata is recovered by checked discovery and sealed into private
  `TraceWork`; drain has no independent claimed type to mismatch. The
  pointer-only `ErasedGc` visitor deliberately treats the owning run's
  canonical metadata as authoritative, while typed root construction retains
  its separate representation-mismatch check.
- **C5D.1 — successful mark publication and report.** Consume a completely
  drained `MarkAttempt` into one scalar `MarkSummary`; this is the point where
  its heap-private bitmap stops being disposable scratch and becomes the
  successful attempt's reachability result. Publish the latest
  `CollectionReport` and matching completed epoch together under the
  coordinator mutex. A failed attempt publishes neither and does not replace a
  prior report. Retain no report history: a synchronous caller overtaken by a
  later completion receives the latest report whose epoch satisfies its
  target. Report root-registry entries, traced objects, distinct marked slots,
  and conservatively retained slots; the last is zero until C6 quarantine.
  Copy no bitmap and add no bitmap-validity flag, identity list, or per-run
  summary. C6 consumes successful marks under the same collection authority;
  live-run and survivor occupancy come from its bitmap scan. C5 reclaims
  nothing.
- **C5D.2 — reachability oracle, scale, and verification closeout.** Compare
  randomized graphs with a simple reachability oracle, run million-edge
  non-recursive tests, include both a million-node deep chain and a flat
  million-edge array, verify zero/one/all-live run bitmaps and repeated
  clear/mark histories, and record peak object-worklist length and capacity
  for the wide fixture without making them correctness thresholds. Update
  `VERIFY.md` with the resulting fixtures and commands. Update `SAFETY.md` only
  with C5 completion and the evidence supporting its existing mark invariants;
  C5D.2 introduces no new unsafe boundary. Run the exact unsafe inventory and
  focused verification suite to prove that inventory remains unchanged. This
  checkpoint is an implementation-verification closeout, not the mandatory
  post-C5 review: it does not reopen C5 architecture, re-audit every unsafe
  block, or reconcile the downstream phase design.

- Stop all mutators, reserve a root-seed worklist, then directly walk and prune
  the stable external-root registry while copying each successfully upgraded
  cell's managed pointer. Release temporary root-cell upgrades during that
  walk and dispatch traces only after the registry operation completes.
- Enumerate runs directly from the heap, never by discovering them through
  thread caches. Mark-only C5 leaves allocation bits, lease bitmaps, stable
  class frontiers, allocation-lease epochs, and provisional C3E pressure
  acknowledgement unchanged. C6A.2a revokes leases and advances the epoch as
  the first transition toward reusing reclaimed storage.
- Mark through each allocation class's edge visitor; do not derive outgoing
  edges from fixed byte offsets. Immediate/non-edge fields are invisible to the
  collector.
- Mark by run slot in its side bitmap; duplicate visits terminate immediately.
  Clear all assigned mark ranges before graph traversal and use a set bit for
  reachability. A successful attempt preserves those bits for C6 dead-set
  classification; exceptional recovery leaves partial bits unpublished until
  the next attempt overwrites them.
- Use an explicit mark stack or queue rather than recursive Rust calls.
- Trace cycles, diamonds, deep chains, wide graphs, and shared logical
  collection spines.
- Treat same-heap edges as a managed-representation invariant: every edge
  reported from an object allocated by heap H must identify a live allocation
  in H. Do not add a validation trace before ordinary `pointer.write`; Glam's
  construction wrappers and the unsafe mutation/`Trace` contract own that
  invariant on the mutation path.
- During the marking trace already required for each reachable object, perform
  an all-build checked owner/slot lookup before dereferencing every reported
  edge. A foreign, stale, non-slot, or otherwise invalid edge is an invariant
  violation and panics rather than returning a recoverable graph error. Debug
  builds may attach richer class and address detail, but the release check is
  mandatory.
- Keep only scalar attempt aggregates. Increment marked-slot and traced-object
  counts when the collector first marks and scans an allocation; do not create
  run-keyed state. C6 later enumerates run side metadata without enumerating
  payloads. Mark traversal remains proportional to reachable managed edges.
- Wrap the attempt in an unwind guard. If tracing or mark-work allocation
  panics, discard the worklist and scalar aggregates, recover the poisoned
  managed-data mutex if the panic crossed trace dispatch while it was held,
  leave every allocation intact, restore a usable non-collecting phase, and
  let the original panic continue to its caller. Partial marks remain
  physically present but have no published result or consumer. A retry clears
  the complete bitmap before tracing, so no bit from the abandoned attempt can
  be mistaken for current reachability.
- Apply that same unwind path to an invalid-edge panic. Marking performs no
  reclamation or destruction, so detection must precede dereference and sweep;
  after unwind both heaps remain intact and ordinary mutation may resume. A
  later collection is expected to panic again until the reachable invariant
  violation is removed. Under `panic = "abort"`, the process terminates without
  collector-induced undefined behavior.
- Do not consume roots or other reachability evidence while marking. Commit
  their retirement only after the corresponding collection succeeds.

Verification includes randomized graph comparison against a simple reference
reachability implementation and million-edge depth tests which cannot overflow
the Rust stack. Deterministic hooks panic after zero, one, and many traced edges;
the caller catches the panic, ordinary mutation resumes, and a later full
collection produces the same survivors as a collection which never failed. A
trace which deliberately panics once must succeed on retry without heap-wide
poisoning. Repeated clear/mark cycles, allocation during the prior finalization
phase, and runs with zero, one, and all slots live receive focused tests. A safe
generic fixture stores a live pointer from heap B in an object in
heap A; collection of A must panic through a checked lookup before dereferencing
the edge or reclaiming anything in either heap. Catching that panic must restore
ordinary admission, while collecting again with the edge still reachable must
panic again. Lease-word Release/Acquire reset verification moves to C6A, where
the collector first publishes reclaimed allocation state.

### C5 completions

#### C5A.0 completion

Completed on 2026-08-23:

- `HeapInner` now owns separate coordinator and managed-data mutexes. Admission
  phase, active-mutator counts, collection epochs, and condition-variable
  predicates live only under the coordinator mutex; arena, classes, pressure,
  roots, and future mark/sweep state live only under the managed-data mutex.
  Production paths release either component guard before acquiring the other.
- The coalesced collection hint is a sibling `AtomicBool`.
  `request_collection` performs one Release store, takes no mutex, and sends no
  notification. Idle admission and synchronous collection inspect the bit with
  Acquire ordering while coordinating election through the coordinator mutex.
- Typed-run pressure stores the request bit while holding managed data.
  Successful collection resets the pressure baseline and clears the bit under
  that same lock, releases managed data, and only then publishes coordinator
  completion. A request before the clear is coalesced; an external request or
  pressure publication after the clear remains pending. Unwind relatches the
  bit before restoring ordinary coordinator state.
- Deterministic tests hold managed data while issuing an asynchronous request,
  pause completion immediately after data-side acknowledgement, and force
  request, mutator entry, root publication, run-pressure publication, and
  synchronous-join schedules on both sides of that boundary. A test-only
  notification counter also proves that request-only transitions do not use
  the condition variable.
- The focused crate verification passes 126 unit tests, 6 Loom models, and 8
  compile-fail/doc tests. Repository-wide formatting, Clippy with warnings
  denied, tests, diff validation, and the exact unsafe inventory also pass.

#### C5A.1–3 completion

Completed on 2026-08-23:

- The collector clears every assigned run's contiguous ordinary-`u64` mark
  range after exclusive admission and before any attempt work. Collector-only
  checked operations test and set exact slot marks; duplicate marks are inert,
  and mutator allocation neither reads nor writes mark state.
- Collector lookup now validates the indexed chunk, exact run and slot, class
  identity and geometry, class run-pool membership, allocation bit, and
  canonical metadata before returning an attempt-local slot description. No
  run-keyed attempt map was introduced; the mark bit remains the only
  per-allocation reachability record.
- Attempt scratch consists only of a stack-scoped worklist and scalar counters.
  The existing collection-attempt RAII guard owns recovery of the managed-data
  mutex and coordinator state. An injected panic discards the scratch,
  recovers mutex poison, relatches the collection request, restores ordinary
  admission, and resumes the original panic without scanning or clearing
  partial marks. The mandatory clear at the beginning of the next attempt
  invalidates those unpublished marks.
- Tests cover zero, one, and three assigned mark ranges; both sides of a bitmap
  word boundary; duplicate marks; allocation independence; exact owner and
  canonical-metadata recovery; rejection of foreign, interior, unallocated,
  absent-class, and unpublished-run addresses; and panics after zero, one, and
  three distinct-run marks followed by a clean retry. Focused verification now
  passes 132 unit tests, 6 Loom models, and 8 compile-fail/doc tests.

#### C5B completion

Completed on 2026-08-23:

- Exclusive collection now observes the stable root-registry length, releases
  managed data, and reserves that much additional worklist capacity before the
  registry walk. The shared retain implementation prunes failed weak upgrades,
  releases each temporary strong cell during the walk, and performs no trace
  dispatch or fallible worklist growth there.
- Every live registry entry increments `root_count`, while checked discovery
  marks the exact allocated slot before enqueueing. Distinct root cells for one
  allocation are counted separately, but clones add no cell and the allocation
  enters the worklist only on its first mark.
- Worklist entries are private `TraceWork` values containing an already-marked
  pointer and the canonical metadata recovered by its checked discovery.
  Exclusive authority preserves that proof, so drain dispatches directly and
  traces the popped object exactly once without another topology lookup.
  Reported edges use the same checked mark-before-enqueue operation, so cycles,
  diamonds, repeated edges, and shared tails terminate without recursion or
  duplicate worklist entries.
- Focused fixtures prove root counting and post-seed trace ordering, a cycle
  combined with duplicate and diamond edges, an unrooted allocation, a
  native 20,000-node chain, and 2,048 branches sharing a 64-node tail. Miri
  exercises the identical chain path with 256 nodes, preserving pointer and
  traversal validation while leaving the stack-depth stress to native
  execution. The crate now passes 136 unit tests, 6 Loom models, and 8
  compile-fail/doc tests.

#### C5C completion

Completed on 2026-08-23:

- Edge-driven worklist publication now performs explicit `try_reserve` growth
  before `Vec::push`. Capacity failure, trace panic, and an injected panic
  between a new edge's mark and its work-item publication all unwind through
  the existing collection-attempt guard. Partial marks remain unpublished
  scratch, the worklist and counters are discarded, managed-data poison is
  cleared, ordinary admission is restored, and the request is relatched.
- Deterministic fixtures panic after zero, one, and many reported edges and
  after zero, one, and many completed worklist pushes. They inspect the exact
  partial bitmap before retry, preserve the original panic payload, and prove
  that a clean retry clears all scratch and traces every reachable allocation.
  The shared graph fixture was corrected to recover its own mutex poison, as
  required by the existing rule that `Trace` remain replayable after its
  visitor panics.
- Adversarial trace fixtures report live foreign-heap, stale-heap, interior,
  and exact-but-unallocated pointers. Checked discovery rejects every case
  before target trace dispatch. The failure repeats while the invalid holder
  remains rooted; releasing it permits collection and subsequent allocation,
  and the live foreign heap remains independently collectable.
- Canonical metadata mismatch is not a separate erased-edge failure mode.
  Pointer-only `ErasedGc` makes no representation claim; checked discovery
  recovers the owning run's canonical metadata and seals it in private
  `TraceWork`. Typed root construction continues to verify an independently
  requested representation.
- Focused verification now passes 141 unit tests, 6 Loom models, and 8
  compile-fail/doc tests.

#### C5D.1 completion

Completed on 2026-08-23:

- A drained `MarkAttempt` is now consumed into one private `MarkSummary`.
  Finishing asserts that no trace work remains, then retains only root-entry,
  traced-object, distinct-mark, and conservative-retention scalars. C5 reports
  zero conservative retention; C6 quarantine will supply the first nonzero
  source.
- Public `CollectionReport` exposes those four counts alongside its epoch.
  `CollectionAttempt::complete` publishes the report and completed epoch in one
  coordinator critical section after all exclusive and finalizer work
  succeeds. Failure keeps the previous report and epoch unchanged.
- The coordinator retains only its latest completed report. A synchronous
  caller waits for its target epoch and returns the latest report satisfying
  that target, allowing a later completion to overtake it without an unbounded
  epoch-indexed history. Entry-elected collections use the same publication
  path even when no caller consumes their report.
- Tests distinguish distinct root cells from clones, duplicate edges from
  distinct marked allocations, and traced live objects from an unreachable
  allocation. Coalesced synchronous callers receive the same nonempty report;
  a forced acknowledgement pause exposes neither report nor completed epoch;
  and a failed trace after epoch one leaves epoch one's report intact until a
  clean epoch-two retry.
- Focused verification now passes 143 unit tests, 6 Loom models, and 8
  compile-fail/doc tests.

#### C5D.2 completion

Completed on 2026-08-23:

- Twenty-four deterministic randomized graph fixtures compare the collector's
  report, every allocation's mark bit, and every trace count with an
  independent index-based reachability oracle. The graphs include cycles,
  duplicates, self-edges, and deliberately unreachable allocations.
- One complete `u64` typed run is driven through zero, one, all, then zero live
  slots across four successful collections. Each report and every allocated
  slot agree, latching that a completed bitmap describes only its own attempt
  rather than accumulated mark history.
- A dedicated serial scale script keeps expensive evidence independently
  measurable from ordinary unit tests. The native marker completes a
  one-million-node chain and a flat one-million-edge array without recursive
  Rust traversal. The flat fixture observed a peak object-worklist length of
  1,000,000 and capacity of 1,048,576; neither value is a correctness or
  performance threshold.
- Test-only peak counters compile out of the collector's production report and
  retain no operational history. The compact test edge representation keeps a
  single chain edge inline, avoiding a million irrelevant host allocations;
  it reuses the existing reviewed `GraphNode` trace contract and adds no unsafe
  site.
- `SAFETY.md` and `VERIFY.md` now record the C5 evidence. The ordinary focused
  suite contains 147 unit tests, of which the two independently timed scale
  fixtures are ignored, plus 6 Loom models and 8 compile-fail/doc tests. The
  exact unsafe inventory remains unchanged. The new fixtures pass focused
  Miri and AddressSanitizer runs, and the complete suite passes
  ThreadSanitizer. At C5D.2 closeout, full AddressSanitizer execution reported
  one 24-byte leak which reproduced unchanged from the clean pre-C5D.2
  `d7977d4` worktree; the mandatory review below attributes it and narrows the
  sanitizer exception instead of silently accepting it.

### Mandatory Post-C5 Review

Completed on 2026-08-23. The review accepts C5 as the exact, non-recursive,
mark-only collector promised by the roadmap; no marking repair is required
before C6.

The implementation audit established the following:

- Every attempt clears all assigned mark ranges before discovering a root or
  synthetic edge. Checked discovery validates exact heap membership, class and
  run topology, allocation state, and canonical metadata, then marks before
  enqueueing. Consequently each managed object enters the LIFO worklist and
  dispatches `Trace` at most once per attempt even through cycles, diamonds,
  duplicate roots, and duplicate edges.
- `TraceWork` deliberately carries both the erased address and the canonical
  metadata recovered by discovery. This is useful proof-carrying attempt state,
  not retained heap metadata: the worklist is drained synchronously under
  exclusive authority and dropped on every failure.
- A failed trace, lookup, worklist reservation, or injected publication point
  publishes neither a report nor a completion epoch. Its partial bitmap is
  scratch which the mandatory next-attempt clear invalidates. Recovery clears
  mutex poison only after the attempt-local worklist and counters unwind, then
  relatches collection and restores ordinary admission.
- `MarkAttempt::finish` requires an empty worklist and reduces the attempt to
  scalar counts. `CollectionAttempt::complete` publishes the latest report and
  matching completed epoch together under the coordinator mutex. No bitmap,
  object identity, per-run summary, or unbounded epoch history escapes.
- C5 changes no allocation bit, lease, class frontier, run pool, or payload.
  Reclamation and durable conservative retention remain C6 work; pressure is
  only acknowledged once the full collection pipeline completes successfully.

The verification evidence is proportionate and independent where it matters:
focused fixtures force duplicate discovery, cross-word boundaries, invalid
edges, trace and work-publication panics, retry, report overtaking, and atomic
publication; a separate index-graph oracle checks complete reachability; the
full-run history checks repeated bitmap clearing; and native million-node and
million-edge fixtures establish non-recursive depth and current worklist
growth. Miri retains bounded versions of the expensive cases, Loom covers the
coordinator models, ThreadSanitizer passes the complete suite, and the unsafe
inventory is unchanged.

The prior full-suite 24-byte LeakSanitizer finding is not a collector or TLS
leak. Exact single-test isolation attributes it to C4D's deliberate
`mem::forget` fixture: forgetting the scoped allocator leaks one inert frontier
cell while proving it cannot retain the heap. The address-sanitizer entry point
now runs every other test with leak detection enabled and that one ownership
fixture separately with ASan enabled but leak detection disabled. The
exception is named and local rather than suppressing unrelated leaks.

The C6-C8 reconciliation made these downstream corrections:

- C6A.0 now owns a no-semantics pipeline refactor. The implemented C5 helper
  exposes only a parameterless exclusive-work callback after consuming the
  mark attempt; C6 instead needs private access to the authoritative bitmap and
  scalar summary under the same collection authority. Classification must not
  grow a second reachability representation merely to work around that seam.
- Stop-the-world frontier retirement no longer claims to race an admitted
  scoped allocator. Exclusive drain proves none exists; the required ordering
  protects internal publication and future admission, while a deliberately
  forgotten allocator is inert and uncallable.
- C5D.2's worklist peaks are test-only evidence. C7C.1 remains responsible for
  any production operational metrics, while the observed million-entry flat
  frontier keeps C8B.3's paged-array tracing investigation justified without
  preselecting its outcome.
- The existing C6 reclamation/finalization checkpoints and the C7-C8 stress,
  metrics, tuning, and audit partitions otherwise remain coherent.

## Phase C6 — Sweep, Mutator Finalization, Retry, and Quarantine

Execute C6 as the following smaller checkpoints:

- **C6A.0 — post-mark collection-pipeline handoff.** Replace the current
  parameterless synthetic `exclusive_work` seam with one collector-private
  post-mark operation which receives the completed scalar mark summary and
  access to managed data while the same collection retains `Exclusive`.
  Reacquiring the managed-data mutex is permitted because exclusive admission
  keeps allocation, root publication, and topology stable; do not copy the
  mark bitmap or retain a managed-data borrow across finalizer admission.
  Preserve C5 behavior exactly: perform no classification or reclamation,
  publish the report only after downstream exclusive/finalizer work succeeds,
  and make a panic relatch collection without publishing an epoch or report.
  Re-run the C5 report, retry, and forced-order fixtures before C6A.1 adds a
  consumer.
- **C6A.1 — dead-set classification without reuse.** From one successful C5
  mark, use C6A.0's private post-mark seam to classify allocated slots and runs
  as live, no-drop dead, or drop-required dead. Publish no free slot or run
  yet. Keep the resulting dead-set plan attempt-local and prove that a panic or
  classification failure leaves allocation, class, frontier, lease, and
  pressure state unchanged.
- **C6A.2a — allocation-lease revocation and epoch publication.** Under
  exclusive collection, revoke every old allocation-word lease and withdraw
  ordinary class-frontier selection for the still-unchanged allocation
  topology, then advance the one heap-wide allocation-lease epoch. The next
  outer cache entry performs one epoch comparison and discards all stale
  cursors. Prove no allocation bit, run record, or reusable slot changes before
  this invalidation boundary. Lease availability and ordinary frontiers are
  rebuilt only after C6A.3's eager sweep.
- **C6A.2b — class frontier and run-pool retirement.** For each wholly dead
  no-drop run, first clear or repoint the old class's atomic frontier, remove
  the stable run record from its class pool, and repair any index encoded by a
  shifted replacement. Exclusive mutator drain proves that no admitted scoped
  allocator or in-flight frontier load remains; clear publication before the
  record is moved so future admission cannot select it. A deliberately
  forgotten allocator is inert and cannot perform another load. Do not reset
  the run header or publish reuse yet.
- **C6A.2c — wholly dead no-drop runs and free-run reuse.** Clear the retired
  run's allocation and side state, reinitialize its empty header, and publish
  it to one heap-wide free list. Prefer recycled runs over virgin arena
  capacity. A run reused during the same exclusive transition may be retyped,
  but its ordinary class frontier remains withheld until C6A.3b publishes the
  completely swept allocator view. Verify cross-class reuse without stale
  class authority.
- **C6A.3a — eager partial no-drop sweep.** Building on C6A.2a's invalidated
  cursor generation, visit the compact allocation and mark words of every
  retained partially live no-drop run and publish `allocated &= marked` while
  the collector remains their sole writer under `Exclusive`. Do not enumerate
  or touch payloads, and do not clear allocation bits in any drop-bearing run.
  Prove boundary words, sparse and dense death, and repeated collections.
- **C6A.3b — swept allocator publication.** Rebuild allocation-word lease
  availability and class frontiers from the eagerly swept topology. Exclude
  every word containing drop-required dead slots in a partially live run, and
  reserve every wholly dead drop-bearing run in full; C6B owns those regions
  until their respective finalization groups are terminal. Publish no ordinary
  allocator selector before all no-drop words are swept. Prove that a
  post-collection claimant observes the final allocation bitmap directly and
  performs no sweep work. The successful mark bitmap has no remaining
  reclamation consumer after dead drop slots and eager no-drop sweep are
  recorded; choose its physical clearing point separately.
- **C6A.4 — assigned-run pressure.** Replace provisional run-publication
  history with assigned-run occupancy, account for virgin and recycled
  activation exactly once, and publish the first survivor-based high-water
  target. This checkpoint does not yet account for allocations made by
  finalizers; C6C.2 publishes the final post-finalization baseline.
- **C6B.1 — finalization batch, reservation, and non-rootability.** Detach
  drop-required dead slots into a collector-owned batch while allocation bits
  still protect their storage. Before reopening admission, reserve every
  affected word in a partially live run from ordinary lease and frontier
  selection. Detach a wholly dead drop-bearing run from ordinary class topology
  in full, retaining its stable run record in the collector-owned batch until
  that run reaches a terminal disposition. The finalizer becomes the sole
  allocation-bitmap writer for every reserved word or run. Extend C4 root
  validation so every batch identity is non-rootable.
  If the batch is empty, either retain the already-proven C3E no-op finalizer
  handoff or take the direct no-finalizer completion path after resolving the
  decision below.
- **C6B.2 — C3E finalizer handoff and destruction.** Use C3E's no-gap
  `Exclusive`-to-`Finalizing` handoff, install the collector's current mutator,
  reopen ordinary admission, and run erased Rust destructors exactly once
  outside collector locks. Group work by allocation word for partial runs and
  by complete run for wholly dead runs so each reserved region can be released
  as soon as its own finalizers are terminal. Successful destruction clears the
  slot allocation bit; fresh allocations and effects remain ordinary
  later-collection state.
- **C6B.3 — non-resurrection and successful completion.** Reject roots to any
  remaining batch identity, permit quining only through fresh allocations,
  and publish completed regions incrementally under heap state. Rebuild and
  republish a terminal partial word immediately. When every destructor in a
  fully reserved run succeeds, retire its saved class record, reset the empty
  run, and publish it immediately to the heap-wide free-run pool; a later
  finalizer or ordinary client may reactivate it without waiting for another
  collection. Prove entry-elected versus synchronous completion transfers or
  drops the finalizer admission exactly as C3E specifies.
- **C6C.1 — panic, draining, and sparse quarantine.** Quarantine a panicking
  slot without invoking its destructor twice, safely drain or classify every
  remaining batch item, retain the first panic for propagation, and restore a
  usable ordinary heap phase. Rebuild every terminal partial word. A fully
  reserved run containing quarantine cannot be retyped: restore its stable
  record to the original allocation class, retain quarantined bits as
  allocated, and publish any remaining safe capacity. Integrate existing
  quarantine identities into every later collection as conservatively live,
  non-traced slots before dead-set classification.
- **C6C.2 — activity, reports, and final pressure publication.** Expose queued
  and running finalizers as heap activity, extend collection reports with
  reclaimed/finalized/quarantined state, incorporate runs activated during
  finalization, and atomically publish the post-finalization occupancy and
  next high-water target.
- **C6D.1 — terminal-teardown decision and forced fixtures.** Choose and record
  either the preferred owner-lease drain or the restricted non-reentrant
  fallback before changing `HeapInner::drop`. C4D already proves that no
  allocator capability owns the heap or survives a drained mutator set. Force
  last-facade, last-authorized-runtime-owner, active scoped-allocator,
  escaped-root, deliberately forgotten inert allocator, mutator-capable
  destructor, panic, and root-attempt orderings against the selected ownership
  graph. An escaped root must neither postpone teardown nor become
  dereferenceable after it, and forgotten allocator storage must not retain the
  heap.
- **C6D.2 — terminal teardown.** Implement only the selected protocol and
  prove each remaining allocation is destroyed or deliberately quarantined
  exactly once without reconstructing a dropped heap owner.
- **C6D.3 — Gate G1 audit.** Reconcile the unsafe inventory, root and
  finalization proofs, Miri, Loom, sanitizers, deterministic panic schedules,
  and terminal heap release. This focused audit closes Gate G1; C7 and C8 add
  stress, metrics, and tuning while integration API work may begin.

Resolve these C6 decisions at the named checkpoint rather than pulling them
into marking:

1. **Successful mark clearing (C6A.3b).** Eager sweep removes every allocator
   dependency on the successful mark bitmap. Decide whether to clear it while
   still `Exclusive` for a tidy post-collection invariant or leave it as stale
   scratch until the next collection's mandatory initial clear. This is a
   physical-state and performance policy, not reachability semantics; tests
   must consult the completed collection report and post-sweep allocator state
   rather than infer validity from residual bits.
2. **Empty finalization batch (C6B.1).** The current C3E pipeline always makes
   the no-gap finalizer handoff, even when its synthetic finalizer does no
   work. The recommended production path skips `Finalizing` when the batch is
   empty and atomically completes `Exclusive` either into the collecting
   entrant's one ordinary admission or into idle `Ordinary` for
   `collect_full`. Keeping the no-op handoff is semantically valid and simpler,
   so forced schedules and a small cost comparison should precede removal.
3. **Survivor growth ratio (C6A.4).** Select the initial internal rational only
   after assigned-run occupancy exists. One half is the provisional comparison
   point, not a contract; threshold correctness tests should accept an injected
   private ratio rather than freeze the measured default into semantics.
4. **Last-owner teardown (C6D.1).** This remains the only blocking ownership
   decision in the isolated collector plan. Do not infer it from ordinary
   collection or implement mutator-capable terminal `Drop` before its forced
   ownership fixtures exist.

Each checkpoint must leave the heap in a usable documented phase after every
recoverable panic injected by its own tests.

### Pressure and Reuse Constraints Carried into C6

- The first successful sweep replaces C2C's historical publication count with
  assigned-run occupancy and the heap-wide recycled-run list described below.
  C3E may acknowledge its synthetic collection by resetting the provisional
  publication counter, but may not publish reclaimed capacity. C6 replaces
  that temporary policy atomically with survivor-based occupancy.
- An abandoned or panicking mark or sweep publishes neither a replacement
  pressure baseline nor recycled capacity. The last known-good request and
  occupancy state remains authoritative.
- Runs activated by finalizers contribute to the consistent post-finalization
  occupancy. Publish the new baseline only after finalization, clear heuristic
  requests coalesced into the collection, and never begin another collection
  while the heap is in `Finalizing`.
- Verify successful and failed sweep publication, free-run reactivation,
  finalizer allocation around the trigger, and panic/quarantine recovery as
  C6 behavior rather than retroactive C2C.5b requirements.

- Identify unreachable allocations only after marking completes.
- Maintain a heap-wide free-run list under heap state. A run enters it only
  after it has no live allocation, every required destructor has completed,
  no quarantined slot remains, its old class frontier and run-pool membership
  are retired, and its header and side bitmaps are safe to reinitialize. A
  no-drop run may reach that state during exclusive sweep; a drop-type run
  cannot enter the list until finalization completes successfully.
- Treat the class's stable run record and raw atomic frontier as one retirement
  unit. Before removing or moving a boxed run record, publish a null or valid
  replacement frontier and repair class bookkeeping. Exclusive mutator drain
  already proves that no admitted scoped allocator or in-flight frontier read
  remains; the publication order protects the allocator view exposed to later
  admission. A deliberately forgotten allocator is inert and cannot execute
  against its stale cell.
- Allocate a new typed run from the free list before consuming a virgin run in
  an existing chunk, and consume existing chunk capacity before allocating a
  new chunk. Reinitialize the run for its new class and geometry before
  publishing a new stable frontier record. The run's numeric location remains
  stable, but no old scoped allocator, frontier, or stale TLS cursor may retain
  authority over its new contents.
- Replace typed-run-publication history with `assigned_runs`, the number of
  runs currently attached to allocation classes, and compare it with
  `arena_chunks * RUNS_PER_CHUNK`. Assigning either a virgin or recycled run is
  one occupancy event; returning a cleared run to the free list decrements the
  occupancy; slot allocation and reuse within an assigned run are invisible to
  this heuristic.
- After a completed collection retains `S` assigned survivor runs, set the next
  automatic high-water mark to the saturating equivalent of
  `S + (RUNS_PER_CHUNK * 7 / 8) + ceil(S * survivor_growth_ratio)`. The fixed
  term preserves C3B's initial 112-run trigger for an empty baseline; the
  proportional term gives high-survivor heaps increasing headroom instead of
  repeating GC after one more run. Keep the ratio as an internal rational
  tuning constant whose initial value is selected during C6A.4 before the
  first pressure baseline is published and measured in C8; neither the ratio
  nor this run-level heuristic is public Glam semantics.
- Trigger on the first run activation which reaches or crosses that absolute
  assigned-run mark. The target may exceed current committed chunk capacity;
  allocate and retain ordinary chunks as required rather than forcing another
  collection merely because the previous capacity was nearly full. Explicit
  requests remain independent of the target.
- Compute assigned survivor-run occupancy during the eager bitmap scan and
  topology update, but publish the final target when the finalization batch
  drains. Runs activated by finalizers join current assigned occupancy before
  the target is published; completion chooses a target above that final
  occupancy rather than latching an immediate follow-up request. A run retained
  by quarantine also joins the baseline because it remains assigned and
  unavailable for recycling. No collection begins during `Finalizing`.
- An abandoned or panicking mark or sweep publishes neither free-list
  membership nor a new pressure baseline. Finalizer-panic recovery publishes
  the consistent survivor/quarantine baseline only after every run has an
  unambiguous assigned, free, or quarantined state.
- Retain every arena chunk until heap destruction, even when all of its runs
  are free. Its capacity remains available to the occupancy calculation and
  free list. Returning or decommitting empty chunks is deferred; the bootstrap
  does not complicate stable chunk indexing for an expected rare case.
- Do not eagerly enumerate every allocated payload. Inspect allocation and
  mark bitmaps directly:
  - a no-drop run with no marked slots is retired wholesale to the free list;
  - a partially live no-drop run clears dead allocation bits eagerly through
    wordwise `allocated &= marked`; and
  - a drop-type run computes its dead slots from `allocated & !marked` and
    queues only those slots for immediate destruction.
- Reclamation is the point at which C5's retained allocation leases become
  stale. Rebuild every live assigned run's lease bitmap, remove retired runs
  from their old class pools, rebuild the class's ready-run order, republish
  class frontiers, and advance the allocation-lease epoch as one exclusive
  transition before making any reclaimed slot available. A run reopened after
  its finalization group completes must enter that same slow-path ready order;
  an older run may not become permanently unreachable merely because the raw
  frontier had advanced to a later record. This is a post-sweep synchronization
  edge, not initial run publication and not part of mark-only C5. Ordinary
  allocation retains its single raw-frontier fast path; ready-order maintenance
  belongs under heap state on cursor exhaustion and finalizer publication.
- Finalizer registration is implicit in homogeneous run metadata. The initial
  design has no per-slot finalizer bitmap and no global finalizer registry.
  Keep allocation bits set, detach the computed dead slots into a
  collector-owned finalization batch, and make those identities non-rootable
  until destruction finishes.
- Before entering `Finalizing`, reserve each affected word in a partially live
  run from ordinary allocation-word leasing and class-frontier selection.
  Fully reserve a wholly dead drop-bearing run and detach its stable record
  from ordinary class topology while still under `Exclusive`; retain that
  record in the finalization batch rather than destroying it early. Allocation
  bitmap updates remain single-writer: the finalizer alone clears or
  quarantines the reserved slots, while ordinary workers may allocate from
  unrelated words and runs concurrently.
- Publish a partial word back to its class as soon as all finalizers in that
  word are terminal. Once an entirely dead run's finalizers all succeed, retire
  its saved record, reset the empty run, and publish it immediately to the
  free-run pool, where a later finalizer or ordinary allocator may retype it.
  If any slot is quarantined, restore the record to its original class instead,
  keep quarantined slots allocated, and publish its remaining usable words.
  Neither case waits for another collection merely to recover the run's safe
  capacity.
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
  state, leaves its allocation bit set and non-reusable, and never invokes its
  destructor again. Fresh allocations and already-published effects from the
  destructor remain valid. Quarantine does not depend on retaining the prior
  successful mark bitmap.
- Before every later dead-set classification, set each sparse quarantined slot
  as live after the initial mark clear and increment the scalar conservative-
  retention count without dispatching its possibly damaged payload's `Trace`
  implementation. This is the only conservative-retention set in the isolated
  collector.
- Terminal teardown consults the same sparse quarantine state and skips every
  destructor identity already recorded there. Quarantine is durable collector
  state, not merely a report field, until the underlying heap storage is
  released.
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
  finalizer mutation is valid. A root created during that drain may protect and
  access its value only while an authorized value-domain owner and matching
  mutator remain live; it does not cancel terminal teardown and becomes inert
  when the domain ends. Terminal teardown ultimately destroys or quarantines
  every remaining allocation regardless of escaped root-registry entries. Do
  not try to reconstruct or resurrect an `Arc` owner after its last strong
  reference has entered `Drop`. If the public ownership representation cannot
  initiate such a drain, keep last-owner teardown a restricted non-reentrant
  path and document which payload families it may destroy; it may not silently
  invoke a mutator-capable production destructor without its promised context.
- Expose queued and running finalizers as operational heap activity so runtime
  quiescence and shutdown cannot race their diagnostics, tasks, or host
  effects. A synchronous `collect_full` report completes only after its
  finalization set has reached terminal object states.
- On completion, publish the post-finalization pressure baseline and clear
  hints coalesced into the completed collection. An entry-elected collector
  atomically carries its finalizer obligation forward as the collecting
  entrant's ordinary mutator obligation; a `collect_full` collector returns
  without one. A request serialized after that transition remains pending for
  a later idle outer entry. No queued collector prevents ordinary admission
  during finalization.

Verification uses drop counters, destructors containing ordinary `Arc`,
`Mutex`, `OnceLock`, and opaque host payloads, scoped current-mutator access,
recursive same-heap entry, bitmap-derived destruction, eager partial-run sweep,
whole-run reclamation, address reuse tests, and Miri checks for stale
references and double destruction. Instrumentation proves an eagerly swept
no-drop partial run is reclaimed from side bitmaps without enumerating its
payload slots. One opaque destructor allocates and publishes a fresh
quine and a diagnostic; the original identity is reclaimed while the published
value survives the next collection. Another schedules work that enters the
same heap from a worker. Uncommitted pressure raised during that finalizer
is coalesced into the completed collection, and a later outer entry proceeds
without an immediate redundant second pass. A barrier-forced request after the
completion transition remains pending and is serviced by a subsequent idle
outer entry. A panicking opaque
destructor quarantines only its slot; allocations and effects it published
before panicking remain valid, and after the caller catches the panic another
allocation and full collection must succeed.

Free-run and pressure verification additionally proves that a wholly cleared
run is detached from its old class before reuse, recycled runs are preferred
to virgin capacity, and a successfully finalized wholly dead run becomes
available to a later finalizer before the overall finalization queue drains.
The same fixture repeats collection and same-type finalizer allocation to prove
that reservation does not force one virgin run per cycle. Each activation
changes assigned occupancy exactly once, and partial or quarantined runs never
enter the free list. Forced threshold tests cover the empty-baseline 112-run
trigger, exact rounding and saturation of the survivor-growth term,
recycled-run crossings, and a high-survivor collection receiving both fixed
and proportional headroom. Finalizer activations consume rather than redefine
that headroom, while quarantine is included in the retained baseline. Heap
destruction releases retained empty chunks; ordinary collection does not.

Gate G1 passes after C6D plus its focused unsafe-code audit.

### Phase C6 Completions

#### C6A.0 completion

Completed on 2026-08-23:

- The collection pipeline now invokes one data-side `post_mark_work` operation
  after `MarkAttempt::finish`. It receives the completed `MarkSummary` and a
  temporary mutable borrow of `ManagedData` while the same collection remains
  `Exclusive`. The mark bitmap stays in its owning runs; no copied live set,
  validity flag, or retained managed-data reference was introduced.
- The callback executes under only the managed-data mutex. Coordinator
  authority is validated before that lock is acquired, and the callback is
  explicitly forbidden from acquiring the sibling coordinator mutex. Its
  guard and borrow end before the existing no-gap finalizer handoff.
- A focused rooted-value fixture observes exact root, trace, mark, and
  conservative-retention scalars alongside the authoritative mark bit. Its
  following finalizer callback reacquires managed data with `try_lock`, latching
  that neither the guard nor borrow crosses admission.
- The prior exclusive-work panic fixture now panics at the post-mark seam and
  still publishes no report or completion epoch, restores ordinary admission,
  and relatches collection. Existing request coalescing, failed-mark retry,
  acknowledgement, finalizer handoff, and scalar-report tests exercise the
  refactored path unchanged.
- C6A.0 adds no unsafe site, allocation-state mutation, classification, sweep,
  lease invalidation, finalization state, or report field. The focused suite
  now contains 148 unit tests, of which the two scale fixtures remain ignored,
  plus 6 Loom models and 8 compile-fail/doc tests. The new handoff fixture also
  passes a focused Miri run, and all workspace checks pass.

### Mandatory Post-C6 Review

## Phase C7 — Shared-Pointer and Worker-Shaped Stress

Execute C7 as five checkpoints:

- **C7A — shared-root and immutable-reader stress.** Hand roots repeatedly
  between workers, clone/drop them around forced collections, and run many
  readers of immutable objects under independent mutator regions.
- **C7B — allocation and coordinator stress.** Force one thread to request or
  synchronously join collection while other threads allocate, block on
  semantic locks outside mutator regions, enter another heap, or unwind. Prove
  that collection waits only for regional mutator obligations and never for a
  pointer-local lock. Use deterministic barriers for each discovered ordering;
  repeated randomized stress remains supplementary.
- **C7C.1 — collection and finalization metrics.** Add entry-elected
  collections, pending and coalesced request observations, synchronous joins,
  pause/trace/sweep/finalization durations, traced objects, reclaimed
  runs/slots, peak mark-object worklist length/capacity, finalization batch
  size, and quarantine outcomes.
- **C7C.2 — allocation and cache metrics.** Add mutator and recursive entries,
  arena chunks, assigned typed runs, recycled and virgin activations, free
  runs, class-cache hits/misses, eagerly swept allocation words and slots,
  fixed-run utilization, and partial-run fragmentation. Track cold
  `TypeId`/metadata discovery separately from retained-class allocation.
- **C7C.3 — metric consistency audit.** Force success, coalesced request,
  abandoned mark, finalizer panic, recycled-run, and terminal paths and prove
  counters describe committed work without double counting. Metrics remain
  operational observations rather than synchronization or correctness state.

This phase tests collector mechanisms only. It does not imitate Glam scheduler
semantics beyond the shape needed to validate shared values.

## Phase C8 — Tuning Surface and Final Collector Audit

Execute C8 as five checkpoints:

- **C8A — tuning and reporting boundary.** Stabilize the explicit collection
  report assembled during C5 through C7 for tests and future runtime metrics.
  Keep collection thresholds and
  similar operational policy as private per-heap tuning. Treat arena-chunk
  size, the single fixed run size, and the direct-mapped worker class-cache
  width as build-time/private-fixture parameters because they participate in
  layout, pointer masking, or compiled TLS shape; do not advertise them as
  runtime heap options. C8 does not introduce variable-size runs.
- **C8B.1 — measurement harness.** Add reproducible allocator, tracing, pause,
  finalization, and reclamation workloads with machine-readable output. Keep
  correctness assertions separate from timing and do not introduce brittle
  unit-test thresholds.
- **C8B.2 — geometry and workload measurements.** Compare selected geometry
  builds or private fixtures, report bitmap bytes and internal fragmentation
  by metadata-requested slot stride, and record representative measurements.
  These observations guide the value layer's later type-layout policy rather
  than becoming public defaults by accident. Measure the repeated assigned-run
  bitmap scan before considering use of the spare `RunHeader` `u32` as a live-
  slot count reset with the mark bitmap. Do not add that count merely to avoid
  an already-cache-local scan; future parallel marking must also account for
  counter contention.
- **C8B.3 — paged array tracing exploration.** Use C5D.2 and C8B's wide-array
  measurements to judge whether the plain LIFO `Vec<TraceWork>` has a material
  peak-memory or reallocation cost. If it does, prototype an additive
  array/range operation on `Visitor` rather than turning every `Trace`
  implementation into a resumable cursor. Keep ordinary bounded tracing
  synchronous. Internally compare a separate stack of erased stable-range
  continuations, processed in private fixed-size pages, with the baseline
  object stack; do not enlarge every ordinary object-stack entry to a range
  enum. Tie delegated range lifetime to the currently traced representation,
  document the unsafe stability obligation, cover embedded arrays and the
  Glam-owned contiguous containers which actually need it, and keep page size
  private and operational. Adopt the extension only if measurement justifies
  its API and unsafe-contract cost; otherwise retain the C5 baseline and record
  the negative result.
- **C8C — final collector audit.** Audit every unsafe block against
  `SAFETY.md`; run Miri, Loom, sanitizers, randomized graph tests, worker stress,
  and all repository checks; reconcile the implementation plan, roadmap,
  verification ledger, and public crate documentation.

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
