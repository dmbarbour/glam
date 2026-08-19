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
| C0 | pending | crate, provenance, safety, and test scaffold |
| C1 | pending | trace, pointer, root, and mutator access contract |
| C2 | pending | non-moving page arena and object metadata |
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
      object.rs              # erased object metadata and headers
      arena.rs               # pages, allocation, free lists
      roots.rs               # explicit root registry
      mark.rs                # full tracing
      sweep.rs               # reclamation and destruction
      generation.rs          # age, remembered sets, minor collection
      metrics.rs             # operational counters and test inspection
  glam-gc-derive/            # only if the derive checkpoint approves it
```

The root manifest becomes a Cargo workspace containing `.` and the approved
path crates while retaining the existing Glam package, library, and binary.
This is implementation support, not a split of the Glam product. The derive
crate joins the workspace only if C1 approves it. Neither collector crate is a
supported embedding API during this transition.

## Initial API Sketch

Names remain provisional until Phase C1:

```rust
pub struct Heap { /* shared heap ownership */ }
pub struct Mutator<'h> { /* !Send region authority */ }
pub struct Gc<T: Trace + ?Sized> { /* Copy, non-rooting */ }
pub struct Root<T: Trace + ?Sized> { /* Clone, Send + Sync as appropriate */ }

impl Heap {
    pub fn with_mutator<R>(&self, f: impl for<'h> FnOnce(&Mutator<'h>) -> R) -> R;
    pub fn request_collection(&self);
    pub fn collect_full(&self) -> CollectionReport;
}

impl<T: Trace> Gc<T> {
    pub fn get<'h>(&self, mutator: &'h Mutator<'h>) -> &'h T;
}

impl<T: Trace> Root<T> {
    pub fn get<'h>(&self, mutator: &'h Mutator<'h>) -> &'h T;
}

pub unsafe trait Trace {
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

## Phase C1 — Trace and Access Contract Spike

Implement only enough allocation leakage to test the public safety shape.

- Decide whether `Gc<T>` carries a heap/domain debug token or relies on page
  ownership checks. Runtime provenance must not enlarge every release pointer
  without measured justification.
- Require a `Mutator` token for safe dereference and allocation.
- Make `Mutator` non-`Send` and non-`Sync`; make `Gc<T>` `Copy`, `Send`, and
  `Sync` only under documented `T` bounds.
- Define the unsafe `Trace` contract, including interior mutability, no hidden
  managed pointers, panic behavior, and destructor restrictions.
- Require tracing to be observational: it may report edges but may not mutate
  the managed graph or collector metadata except through its visitor. A panic
  from an otherwise conforming trace may abort that collection attempt without
  making the graph unsafe to trace again.
- Define a small managed-edge mutation gateway whose collector action is an
  inline no-op for full stop-the-world collection. This latches the API shape
  without imposing remembered-set or concurrent-marking work prematurely.
- Prototype manual tracing and a derive macro. Approve a derive crate only if
  generated implementations are inspectable and cover enums, arrays, options,
  tuples, and common containers without broad unsafe escape hatches.
- Specify equality, pointer identity, debugging, and heap-mismatch behavior.

Verification:

- compile-fail tests show that references cannot escape a mutator region and a
  mutator cannot be sent to another thread;
- shared `Gc` and `Root` handles may move between threads;
- a pointer cannot be safely dereferenced with authority from another heap;
- tracing a duplicate pointer is harmless; and
- derived and manual traces visit the same edge multiset in representative
  recursive enums.

Checkpoint: freeze the internal API before building the allocator. Revisit the
integration plan if using the token in real evaluator call paths would require
an unacceptable semantic or visibility change.

## Phase C2 — Non-Moving Arena and Object Metadata

- Allocate stable, aligned, typed objects from heap-owned pages.
- Store type-erased trace, drop, size, and alignment operations in reviewed
  metadata or a vtable.
- Separate object liveness metadata from payloads where practical so later
  page bitmaps remain possible.
- Reserve generation/age representation without yet implementing minor
  collection.
- Support sized allocations first. Keep byte arrays and other DSTs outside the
  managed heap until a concrete Glam need justifies them.
- Require mutator authority for every managed allocation.
- Give each outer mutator entry a lazily acquired local allocation region.
  Recursive same-heap entries share it. Ordinary objects bump an aligned local
  cursor without locks or atomic operations.
- Obtain another region only when the local region is first needed or becomes
  exhausted. Prefer an atomic reservation from the current large arena page;
  use `HeapState` synchronization for slow paths such as installing another
  large page, recycled-span coordination, and oversized allocations.
- Keep arena-page and mutator-region sizes tunable. Values such as 8–64 MiB for
  a large page are plausible starting measurements, not semantic constants;
  the local region should be much smaller so one short-lived mutator cannot
  strand an arena page.
- Accept or otherwise construct the payload before making it collector-visible,
  reserve aligned storage by advancing the private cursor, initialize its
  header and payload, and only then mark that object committed. Reserved but
  unused or uncommitted bytes contain no traceable object. On outermost mutator
  exit, publish the completed-region watermark/metadata and return or account
  for the unused tail before decrementing the active-mutator count.
- Charge collection pressure when regions or large objects are reserved, then
  reconcile useful and unused bytes at outer exit. Ordinary allocation should
  not increment a shared byte counter.
- Permit a simple mutex-backed slow path initially, but do not put the global
  heap mutex on every ordinary allocation. Changing reservation and recycling
  algorithms later must not change the mutator or pointer contract.

Verification:

- mixed sizes and alignments retain stable addresses;
- page rollover and large-object fallback are correct;
- allocation from several mutators never overlaps;
- allocations within one reserved region require no shared synchronization;
- recursive entries consume the outer entry's region and only its outer exit
  returns the unused tail;
- panic unwinding returns or safely abandons a partially used region without
  exposing uninitialized storage as an object;
- pointer ownership checks distinguish heaps in debug/test builds; and
- dropping the heap without collection destroys every allocation exactly once.

## Phase C3 — Regional Mutators and Stop-the-World Handshake

- Implement outer `enter`/`exit` admission with a heap phase mutex/condition
  variable or equivalent state machine.
- If admission and allocation slow paths share one `HeapState` mutex, keep
  their fields and transitions separately documented. The mutex belongs to
  shared page, region-recycling, and phase state; it is not held by a
  mutator's local bump allocator. The collector sets its request under that
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
- Outermost exit publishes allocation-region metadata before publishing the
  active-count decrement. That release/acquire edge is part of the collector's
  visibility proof and must be exercised under Loom or deterministic barriers.
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
- Mark by allocation identity; duplicate visits terminate immediately.
- Use an explicit mark stack or queue rather than recursive Rust calls.
- Trace cycles, diamonds, deep chains, wide graphs, and shared logical
  collection spines.
- Validate that every traced pointer belongs to the collecting heap in debug
  and test configurations.
- Choose an epoch or bitmap reset strategy which cannot confuse an old mark
  with the current collection.
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
poisoning.

## Phase C6 — Sweep, Mutator Finalization, Retry, and Quarantine

- Identify unreachable allocations only after marking completes.
- While the world is stopped, partition the completed dead set into storage
  which needs no Rust destruction and allocations requiring finalization.
  Reclaim the former without moving survivors. Detach the latter into a
  `FinalizationQueued` set whose identities can no longer be rooted or returned
  to `Allocated`, but whose storage remains intact until its destructor runs.
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
  dying allocation. The same rule prevents rescuing another
  `FinalizationQueued` allocation through a stale internal pointer.
- Give each finalizable allocation an explicit transition such as `Allocated
  -> FinalizationQueued -> Finalizing -> Free` (exact names provisional). A
  panic in one payload's destructor transitions that allocation to a terminal
  non-reusable `Quarantined` state and never invokes its destructor again.
  Fresh allocations and already-published effects from the destructor remain
  valid.
- Specify and test how a destructor panic drains or preserves the remaining
  finalization queue. Before resuming the panic, the implementation must leave
  no allocation ambiguously owned and must restore an ordinary heap phase; a
  queued collection may run only after this recovery. The bootstrap should
  prefer quarantining the failed allocation and safely draining the remaining
  queue, while retaining the first panic for propagation.
- Treat a panic or assertion showing that shared allocator metadata may be
  partially mutated as an internal collector defect. Poison or abort only when
  an attempt guard cannot prove a consistent phase, page, object-state, and
  free-list boundary.
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
recursive same-heap entry, address reuse tests, and Miri checks for stale
references and double destruction. One opaque destructor allocates and
publishes a fresh quine and a diagnostic; the original identity is reclaimed
while the published value survives the next collection. Another schedules work
that enters the same heap from a worker. Uncommitted pressure raised during
that finalizer remains coalesced. A separate deterministic test commits the
next collection during finalization and proves that writer priority blocks a
later mutator, without beginning its trace until the finalizer exits. A
panicking opaque destructor quarantines only its allocation; allocations and
effects it published before panicking remain valid, and after the caller
catches the panic another allocation and full collection must succeed.

Gate G1 passes after C6 plus a focused unsafe-code audit.

## Phase C7 — Shared-Pointer and Worker-Shaped Stress

- Exercise roots handed repeatedly between worker threads.
- Exercise many readers of immutable objects under independent mutator
  regions.
- Exercise one thread requesting collection while other threads allocate,
  block on semantic locks, or unwind.
- Confirm that collection never waits on a pointer-local lock.
- Add metrics for mutator entries, recursive entries, pauses, pages, traced
  objects, reclaimed objects, and deferred requests.

This phase tests collector mechanisms only. It does not imitate Glam scheduler
semantics beyond the shape needed to validate shared values.

## Phase C8 — Generational Metadata and Barrier API

- Divide allocation state into young and old generations without moving
  objects.
- Treat ordinary mutator-local allocation regions as young. Old-generation
  allocation is limited to promotion policy and any reviewed large-object
  path, rather than selected independently by each ordinary allocation.
- Select per-object aging versus whole-page promotion through a measured design
  checkpoint. Mixed-generation pages are acceptable if their metadata and
  sweep cost remain clear.
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

Verification proves remembered-set publication ordering with deterministic
barriers and compares full tracing with a reference graph after arbitrary
mutations.

## Phase C9 — Minor Collection and Promotion

- Stop all mutators.
- Trace young objects from external roots and remembered old objects.
- Reclaim unreachable young objects and promote survivors according to the C8
  policy.
- Retire remembered entries only when their owner is rescanned or no longer
  contains a young edge, and commit that retirement only after the minor
  collection succeeds. A failed minor mark retains all prior reachability
  evidence for retry.
- Fall back to full collection on generation overflow, remembered-set pressure,
  or an invariant check which cannot be answered cheaply.

Differential verification runs the same generated allocation/mutation history
through minor collections and full-only collections, then compares reachable
payloads, destruction counts, and pointer identities.

Gate G4 cannot pass here alone; the Glam integration barrier inventory must
also pass.

## Phase C10 — Tuning Surface and Final Collector Audit

- Expose internal tuning for page sizes, collection thresholds, nursery size,
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
- Unsafe contracts and copied-code provenance are auditable in the subcrate.
- No collector API depends on Glam `Value`, scheduling, reflection, or host I/O.
- Concurrent marking is possible without changing pointer representation, but
  remains disabled and unimplemented until separately planned.
