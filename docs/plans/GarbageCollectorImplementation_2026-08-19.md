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
| C6 | pending | sweeping, destruction, and poisoning |
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

The API must not promise `Deref`, finalizers, moving collection, weak pointers,
or arbitrary cross-heap conversion.

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
- Make allocation local to a mutator; use per-mutator bump state or page leases
  so ordinary allocation does not take one global lock.

Verification:

- mixed sizes and alignments retain stable addresses;
- page rollover and large-object fallback are correct;
- allocation from several mutators never overlaps;
- pointer ownership checks distinguish heaps in debug/test builds; and
- dropping the heap without collection destroys every allocation exactly once.

## Phase C3 — Regional Mutators and Stop-the-World Handshake

- Implement outer `enter`/`exit` admission with a heap phase mutex/condition
  variable or equivalent state machine.
- Support recursive same-heap entry through thread-local depth without
  incrementing the global active-mutator count.
- Reject or explicitly diagnose nested entry into a different heap.
- A collection request prevents new outer entries, then waits for every active
  mutator to exit.
- Allocation thresholds request collection but do not synchronously collect
  from the middle of a mutator region.
- Elect exactly one collector; other requesters wait for or observe its epoch.
- Specify panic unwinding from a mutator closure and ensure outer exit still
  publishes quiescence.

Deterministic tests must force:

1. request immediately before a mutator enters;
2. request while one or several mutators are active;
3. nested entry while a request is pending;
4. last mutator exit racing a second requester;
5. a panicking mutator; and
6. heap drop while collection waiters exist.

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

Verification includes randomized graph comparison against a simple reference
reachability implementation and million-edge depth tests which cannot overflow
the Rust stack.

## Phase C6 — Sweep, Destruction, and Heap Poisoning

- Identify unreachable allocations only after marking completes.
- Reclaim slots/pages without moving survivors.
- Invoke erased Rust destruction exactly once under a documented phase which
  permits no managed allocation, evaluation, host callback, or collector
  re-entry.
- Do not run user-visible finalization.
- Decide and test panic policy. The preferred bootstrap policy is to poison and
  conservatively leak uncertain allocations during unwinding, then reject
  further use; abort is acceptable if recovery cannot be proved sound.
- Reuse reclaimed storage only after destruction and metadata retirement are
  complete.

Verification uses drop counters, destructors containing ordinary `Arc`,
`Mutex`, and `OnceLock` values, address reuse tests, and Miri checks for stale
references and double destruction.

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
- Select per-object aging versus whole-page promotion through a measured design
  checkpoint. Mixed-generation pages are acceptable if their metadata and
  sweep cost remain clear.
- Add a small write-barrier API which can dirty an old owner before or while a
  young edge becomes visible.
- Provide collector-aware wrappers or helper operations for the publication
  patterns Glam actually uses: replaceable fields, `Mutex`-protected fields,
  and one-time cells.
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
  contains a young edge.
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
