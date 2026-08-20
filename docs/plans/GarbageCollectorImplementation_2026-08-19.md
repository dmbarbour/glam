# Glam GC Subcrate Implementation Plan — 2026-08-19

Status: planned.

This plan implements an exact, non-moving, runtime-local tracing collector
without depending on Glam value semantics. The governing requirements and
integration gates live in
[`GarbageCollectionRoadmap_2026-08-19.md`](GarbageCollectionRoadmap_2026-08-19.md).

The initial deliverable is a stop-the-world full collector. Generational
storage and minor collection follow as separately admitted functionality.
Concurrent marking is a later plan.

## Phase Status

| Phase | Status | Outcome |
| --- | --- | --- |
| C0 | completed | crate, provenance, safety, and test scaffold |
| C1 | pending | trace, pointer, root, and mutator access contract |
| C2A | pending | arena chunks, typed-run geometry, and layout limits |
| C2B | pending | type metadata and per-heap allocation-class discovery |
| C2C | pending | worker-local typed-run allocation and reuse |
| C3 | pending | recursive mutator regions and STW handshake |
| C4 | pending | explicit external roots |
| C5 | pending | exact full marking |
| C6 | pending | sweeping, mutator finalization, retry, and quarantine |
| C7 | pending | shared-pointer and worker-shaped stress |
| C8 | pending | generational metadata and barrier API |
| C9 | pending | minor collection and promotion |
| C10 | pending | tuning and final collector audit |

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
      generation.rs          # age, remembered sets, minor collection
      metrics.rs             # operational counters and test inspection
```

The root manifest becomes a Cargo workspace containing `.` and the approved
path crates while retaining the existing Glam package, library, and binary.
This is implementation support, not a split of the Glam product. The collector
crate is not a supported embedding API during this transition.

## Initial API Sketch

Names remain provisional until Phase C1:

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
  `Mutator` is non-`Send` and non-`Sync` but offers no operation. C1 may still
  change token and provenance representation before managed pointers exist;
- `SAFETY.md` records an empty unsafe inventory, latched by compiling every
  crate target and feature with `unsafe_code` forbidden; and
- stable checks and the Loom API smoke model run through the crate-local
  verification script. Miri and sanitizer scripts are present but require an
  appropriately installed nightly toolchain.

## Phase C1 — Trace and Access Contract Spike

Implement only enough allocation leakage to test the public safety shape.

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
- Implement tracing manually for the C1 representative graph types. Provide
  narrowly reviewed structural implementations or visitor helpers for wrappers
  which contribute no representation policy of their own, initially options,
  fixed arrays, tuples, and slices as actual use requires. Do not add a derive
  crate or generic field-reflection abstraction in this plan: Glam has a
  bounded representation inventory, and persistent collections,
  synchronization cells, roots, external storage, and immediate fields require
  explicit edge policy.
- Specify equality, pointer identity, debugging, and heap-mismatch behavior.
- Keep raw `Gc<T>` construction private and unsafe. Specify the obligations
  shared by allocator implementations and any future representation decoder:
  the address is non-null and properly aligned, identifies a live managed slot
  in the protected heap, and the slot's canonical metadata describes `T`.
  `glam-gc` does not specify a tagged value, tag-to-type mapping, or serialized
  representation.
- Reserve a bounded run-size-class representation in the pointer/access layer
  without assigning Glam immediate tags. The collector API accepts untagged
  aligned addresses plus its own run-owner information and does not expose the
  eventual Glam `Value` encoding.

Verification:

- compile-fail tests show that references cannot escape a mutator region and a
  mutator cannot be sent to another thread;
- shared `Gc` and `Root` handles may move between threads;
- a pointer cannot be safely dereferenced with authority from another heap;
- forced wrong-heap and wrong-representation accesses trip debug assertions at
  the unsafe gateway without adding fields or release checks to `Gc<T>`;
- tracing a duplicate pointer is harmless; and
- manual traces visit an independently stated expected edge multiset for
  representative structs, recursive enums, and each admitted structural helper.

Checkpoint: freeze the internal API before building the allocator. Revisit the
integration plan if using the token in real evaluator call paths would require
an unacceptable semantic or visibility change.

## Phase C2A — Arena Chunks, Typed-Run Geometry, and Layout Limits

- Reserve large heap-owned arena chunks and divide them into a bounded table of
  power-of-two run sizes. Every run is aligned to its own size and contains one
  homogeneous slot layout.
- Require at least 32-byte managed-slot alignment. Select the exact run-size
  table through a checkpoint, with at most the run-class count reserved by C1;
  8–16 classes are the intended scale, not a semantic requirement.
- Given an untagged managed address and its run-size class, recover the run
  header by masking. The safe access layer validates that the run belongs to
  the expected heap and allocation class in debug/test configurations.
- Store allocation and mark state in run-side bitmaps. Reserve run-side card
  metadata and a generation field without implementing minor collection.
- Set a documented maximum managed size and alignment derived from the largest
  supported slot/run class. Reject an unsupported type when creating its
  allocation class. Do not implement a large-object fallback, multi-run object,
  heterogeneous run, or arbitrary DST path.
- Keep variable-sized byte buffers, arbitrary host payloads, and other values
  which do not fit the limit in audited external storage or decompose them in a
  later Glam representation project.
- Keep the header-address formula private. Expose only checked owner/class
  lookup needed by pointer access and the future tagged-value layer.

Verification:

- every boundary address in every run-size class maps to exactly one header;
- adjacent runs and arena chunks never alias;
- bitmap indices match 32-byte slot quanta and reject interior/non-slot
  addresses;
- unsupported size/alignment/DST layouts fail class creation without partially
  allocating storage;
- pointer ownership checks distinguish heaps in debug/test builds; and
- Miri and property tests cover mask arithmetic, alignment, and the maximum
  supported address range.

## Phase C2B — Type Metadata and Per-Heap Allocation-Class Discovery

- Define immutable object metadata containing layout, visitor dispatch, and an
  optional erased `Drop` operation. Intern exactly one descriptor per Rust type
  in a process-wide registry keyed on `TypeId`, and use the winning
  `&'static ObjectMetadata` address as the operational type identity. `TypeId`
  is a cold discovery key, not a run-header field or hot-path comparison. Do
  not add relocation operations while moving collection is deferred.
- Give each typed run one metadata/allocation-class identity in its header;
  ordinary slots contain payload only, with no GC header, metadata pointer,
  mark byte, or finalizer byte.
- Maintain a per-heap map from canonical metadata pointer to
  `AllocationClassId` for first-use class discovery and a stable dense class
  table containing that metadata pointer and typed-run pools. Metadata function
  bodies are monomorphized for `T`; the metadata address is the canonical Rust
  type identity while the dense class entry is the canonical heap-local
  allocation identity.
- Return a reusable `AllocationClass<T>` handle after discovery. Its heap
  provenance, metadata pointer, and dense class ID make subsequent worker
  allocation independent of `TypeId` lookup or hashing.
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
- repeated allocation through a retained class performs no `TypeId` lookup;
- no-drop and drop types receive the correct metadata without per-slot policy;
  and
- failed or panicking metadata/class construction publishes no partial entry.

## Phase C2C — Worker-Local Typed-Run Allocation and Reuse

- Require mutator authority and an `AllocationClass<T>` for every managed
  allocation.
- Give each outer mutator entry a small local cache from dense class ID to its
  current run cursor. Recursive same-heap entries share that cache.
- Allocate ordinary slots from the cached run using only worker-local cursor or
  free-bitmap state. Do not look up or hash `TypeId`, acquire a shared lock, or
  increment a shared byte counter on the ordinary hot path.
- On cache miss or run exhaustion, use the class's synchronized slow path to
  obtain a reusable partial run or reserve a new typed run from an arena chunk.
  Return/publish partial cursors in a batch at outer mutator exit.
- Initialize the payload completely before setting its allocation bit. A
  reserved but uncommitted slot is not traceable. Panic unwinding returns the
  slot to local free state without invoking `Drop` on uninitialized bytes.
- Charge allocation pressure when runs are obtained and reconcile unused slots
  when partial runs are returned. Tune the number and representation of local
  class cursors only after allocation histograms exist.
- Permit a mutex-backed run-turnover path initially. Changing run-pool or local
  cache policy later must not change pointer, trace, or mutator semantics.

Verification:

- allocation from several mutators never overlaps;
- instrumentation proves repeated allocations in a cached class perform no
  hash lookup or shared synchronization;
- recursive entries reuse the outer cache and only its outer exit publishes
  partial runs;
- cold types do not lose their partial runs permanently when a mutator exits;
- reused slots are correctly reinitialized and marked allocated;
- panic unwinding never exposes uninitialized storage as an object; and
- dropping the heap without collection destroys every allocated drop-type slot
  exactly once.

## Phase C3 — Regional Mutators and Stop-the-World Handshake

- Implement outer `enter`/`exit` admission with a heap phase mutex/condition
  variable or equivalent state machine.
- If admission and allocation slow paths share one `HeapState` mutex, keep
  their fields and transitions separately documented. The mutex belongs to
  arena-chunk, typed-run-pool, class-discovery, and phase state; it is not held
  by a mutator's local run cursor. The collector sets its request under that
  mutex, waits for the active count to reach zero, and then has exclusive
  access to allocation state without retaining a mutator-region lock.
- Support recursive same-heap entry through thread-local depth without
  incrementing the global active-mutator count.
- Provide a scoped current-mutator accessor for reviewed destructor and
  runtime-integration code, for example an HRTB closure API rather than a
  borrow which can escape. A destructor invoked by the collector sees its
  finalizer mutator as current; a same-heap public runtime operation therefore
  re-enters recursively instead of acquiring independent admission.
- Reject or explicitly diagnose nested entry into a different heap.
- A collection request prevents new outer entries, then waits for every active
  mutator to exit.
- Give the collector a privileged collector-to-mutator handoff. After marking
  fixes the dead set, and before releasing exclusive mutator admission, the
  collector acquires one ordinary mutator lease for its own thread. With an
  `RwLock`-shaped barrier this is an atomic write-to-read downgrade; with an
  active-count state machine it increments the active count and publishes
  `Finalizing` while still holding the coordinator lock. There must be no
  interval in which neither collector exclusion nor the finalizer mutator is
  authoritative.
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
  deliberately use writer preference. New mutators then wait behind the
  collection because the heuristic has selected stop-the-world work as the
  runtime's next priority. Do not weaken that priority merely to improve reader
  throughput; tune when collection is committed instead.
- Consequently, an already-admitted mutator must be able to reach its outer
  exit without synchronously depending on a new outer mutator admission.
  Recursive same-thread entry remains available. Work which truly requires a
  new worker must either establish that admission before commitment, be left
  scheduled for after collection, or keep the heuristic from committing yet.
  This is a general stop-the-world mutator contract, not a finalizer-specific
  exception.
- Mutators admitted before the request may continue allocating while they
  finish their bounded region; those allocations remain part of the heap which
  the collector sees after the active count reaches zero. A pending request
  must not strand an admitted mutator before it can exit.
- Outermost exit publishes completed allocation bits and returns/publishes all
  local typed-run cursors before publishing the active-count decrement. That
  release/acquire edge is part of the collector's visibility proof and must be
  exercised under Loom or deterministic barriers.
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
3. nested entry while a request is pending;
4. last mutator exit racing a second requester;
5. collector-to-mutator handoff racing coalesced collection pressure;
6. a finalizer waiting for a worker mutator before that pressure is committed;
7. commitment of the next collection blocking a new mutator behind its writer;
8. a panicking mutator; and
9. heap drop while collection waiters exist.

Use Loom for the coordination state where feasible. Repeated stress is
supplementary, not proof.

## Phase C4 — External Root Registry

- Implement a shareable root cell registered once with its heap.
- Cloning a root may use ordinary atomic ownership at this external boundary;
  internal `Gc` copies remain free of that cost.
- Root destruction must not acquire an allocator lock or race into premature
  reclamation.
- Root creation from `Gc` is permitted only within a mutator region.
- Root access enters or requires the correct heap.
- The registry may retain weak root slots and prune them during a pause; it
  must not retain dead root payloads indefinitely.

Verification forces root creation, cloning, final drop, and registry pruning
around every root-snapshot boundary. A root cloned from an existing root during
a pause remains safe; no new root can arise from an otherwise unreachable
bare pointer while mutators are stopped.

## Phase C5 — Exact Full Marking

- Stop all mutators and snapshot/visit external roots.
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
  without enumerating its payloads. Marking may touch page/run metadata, but its
  graph traversal remains proportional to reachable managed edges.
- Wrap the attempt in an unwind guard. If tracing or mark-work allocation
  panics, discard the worklist, leave every allocation intact, restore a usable
  non-collecting phase, and let the panic continue to its caller. A retry uses
  a fresh epoch, so marks from the abandoned attempt are irrelevant.
- Do not consume roots, remembered-set entries, or other reachability evidence
  while marking. Commit their retirement only after the corresponding
  collection succeeds.

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

Gate G1 passes after C6 plus a focused unsafe-code audit.

## Phase C7 — Shared-Pointer and Worker-Shaped Stress

- Exercise roots handed repeatedly between worker threads.
- Exercise many readers of immutable objects under independent mutator
  regions.
- Exercise one thread requesting collection while other threads allocate,
  block on semantic locks, or unwind.
- Confirm that collection never waits on a pointer-local lock.
- Add metrics for mutator entries, recursive entries, pauses, arena chunks,
  typed runs, class-cache hits/misses, traced objects, reclaimed runs/slots,
  lazy sweeps, and deferred requests.

This phase tests collector mechanisms only. It does not imitate Glam scheduler
semantics beyond the shape needed to validate shared values.

## Phase C8 — Generational Metadata and Barrier API

- Divide typed runs into young and old generations without moving objects.
  Ordinary newly obtained runs are young.
- Use whole-run generation and promotion initially. A promoted partial run may
  serve only old allocation or remain closed to new allocation; do not mix
  young and old slots merely to recover space.
- Add a card bitmap to old runs. Select a coarse card size from the same
  measured run geometry rather than adding remembered state to every object.
- Add a small write-barrier API which can dirty an old owner before or while a
  young edge becomes visible. This activates generational behavior in the
  mutation gateway established by C1; it is required for minor collection even
  though the collector still stops all mutators while tracing.
- Provide collector-aware wrappers or helper operations for the publication
  patterns Glam actually uses: replaceable fields, `Mutex`-protected fields,
  and one-time cells.
- Do not implement an already-marked-object barrier in this phase. That belongs
  to a separately planned incremental or concurrent marker.
- Keep barriers idempotent and thread-safe; no barrier is required for fields
  which cannot contain managed pointers.

Verification proves card publication ordering with deterministic barriers,
checks boundary writes for every run/card size, and compares full tracing with
a reference graph after arbitrary mutations.

## Phase C9 — Minor Collection and Promotion

- Stop all mutators.
- Trace young objects from external roots and dirty cards in old runs.
- Reclaim wholly dead young runs, lazily recover dead slots in partial no-drop
  runs, finalize dead drop-type slots, and promote survivors according to the
  whole-run C8 policy.
- Clear card bits only when the corresponding old region has been rescanned and
  commit that clearing only after the minor collection succeeds. A failed
  minor mark retains all prior reachability evidence for retry.
- Fall back to full collection on generation overflow, remembered-set pressure,
  or an invariant check which cannot be answered cheaply.

Differential verification runs the same generated allocation/mutation history
through minor collections and full-only collections, then compares reachable
payloads, destruction counts, and pointer identities.

Gate G4 cannot pass here alone; the Glam integration barrier inventory must
also pass.

## Phase C10 — Tuning Surface and Final Collector Audit

- Expose internal tuning for arena-chunk size, the bounded run-size table,
  worker class-cache capacity, card size, collection thresholds, nursery size,
  promotion, and full/minor selection without exposing collector jargon as a
  stable Glam public API.
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
cargo +nightly miri test -p glam-gc
```

Run supported address/thread sanitizers in CI or a documented local script.
Every concurrency defect receives a forced-order regression before repair.

## Collector Completion Criteria

- Pointer copying and reading acquire no pointer-local collector lock.
- One heap supports multiple concurrent mutators and shared roots.
- Full collection is exact, non-moving, and cycle collecting.
- Minor collection is observationally equivalent to full collection.
- Every managed allocation fits one documented typed-run class; unsupported
  layouts are rejected without a hidden fallback.
- Unsafe contracts and copied-code provenance are auditable in the subcrate.
- No collector API depends on Glam `Value`, scheduling, reflection, or host I/O.
- Concurrent marking is possible without changing pointer representation, but
  remains disabled and unimplemented until separately planned. Moving remains
  a separate future design rather than a completion claim of this plan.

Trace derive macros remain deferred. Reconsider one only if the Glam integration
inventory demonstrates substantial mechanical visitor repetition, and treat it
as an independently audited maintenance tool rather than a collector gate.
