# Value Representation Refinement Plan — 2026-08-19

Status: preliminary and deferred. Its original gate, a working Glam-owned GC
boundary, was met when roadmap Gate G4 passed on 2026-10-02. The holistic
pre-performance review now sequences it after that review's measurement and
overhead-removal steps, so that it measures representation rather than lock
and scheduler overhead. Its findings V1–V3 and V5 are the V-1 prework phase
below.

The deferred
[Pure Effect Access Fusion plan](PureEffectAccessFusion_2026-09-23.md) follows
this transition so it can measure and optimize the refined value, edge, and
root-publication costs rather than the temporary compatibility representation.
That dependency adds no effect-fusion work to this plan.

## Purpose

Replace the current large Rust `core::Value` enum with a compact internal value
handle after runtime-local tracing ownership is established. The intended
direction is a tagged immediate-or-managed-pointer word, with common managed
nodes using measured compact slot sizes. Plausible 16-, 24-, and 32-byte nodes
must be compared rather than fixing one target in advance; exceptional values
may use larger allocation classes.

This is a performance and representation transition. It must preserve Glam
evaluation, identity, confluence, reflection, diagnostics, and public API
semantics.

## Why This Is Separate From GC

The collector needs to know how to:

- enumerate managed edges through a visitor without freezing a public offset
  representation;
- find allocation/run metadata from an untagged managed pointer;
- honor Rust payload layout plus an optional larger slot size in canonical
  object metadata; and
- root values at the Rust API boundary.

It does not need to know Glam's immediate tags, numeric encoding, builtin
encoding, semantic node taxonomy, or how many low pointer bits Glam wants.
Conversely, this plan expresses stronger pointer alignment through its Rust
node types or common wrappers, and expresses extra slot padding through object
metadata. It may rely on those layouts and the owner-lookup contract, but must
not reach into collector-private run tables or bitmap representations.
Because Rust alignment attributes are part of a concrete type's layout, a
common managed-node wrapper or declaration macro is the central policy point;
the alignment is not a runtime-global variable.

The GC implementation may begin with ordinary typed `Gc<T>` pointers and a
larger `core::Value`. Compact tagged values are not a prerequisite for exact
collection.

The tentative
[`GarbageCollectorScopedPointerSafety_2026-09-09.md`](GarbageCollectorScopedPointerSafety_2026-09-09.md)
plan considers exposing copyable mutator-branded working views. Persistent
managed edges are already move-only and duplicate explicitly under matching
access after the completed D.2h/P4 cutover. Review only the additional scoped
working-view model alongside the final internal `Value` copy policy before
V2-V4; do not migrate today's compatibility representation merely to pre-empt
that decision. The same review decides whether `Gc<T>` stays `Send` and
`Sync`, once scoped working views and real cross-thread persistent owners are
inventoried.

## Current Pressure

The current enum stores representations such as `Number(BigRational)` inline.
Its size is therefore determined by its largest variant rather than by the
common atom, builtin, small integer, or managed-pointer cases. It also combines
semantic value classification with Rust ownership glue such as `Arc`, making
trivial reclamation and compact copying difficult.

**Allocation volume, measured 2026-10-08.** After the recursion work, the
allocator (`malloc`, `free`, `memmove`) takes about 14% of samples in the
countdown. It is diffuse: root registration, vector growth, finalization,
persistent-map nodes and continuation frames. A faster allocator is not a
free win: mimalloc ran 9–16% fewer instructions but used more CPU in this
container (`perf-mimalloc-allocator` in the
[structural overheads plan](StructuralOverheads_2026-10-08.md)). Compact
values and list and dictionary representations should allocate less.
Root registration is the clearest single source, and is part of
`perf-allocation-path` there.

**Unmanaged structure the collector traces, measured 2026-10-09.** Lists
and dictionaries are still `Arc` structure. Each collection traces them,
but they occupy no managed runs, so they do not raise the next collection's
threshold. A program that holds a growing list collects at a rate set by
its managed allocation, and each collection traces the whole list:
`append_walk` collects 4, 8 and 14 times at 800, 1,600 and 3,200 items.
Managed list nodes would count toward the threshold, so that collections
grow with the live data rather than with the run. Generational collection
is the eventual mitigation for long-lived structure that every full
collection traces (maintainer, 2026-10-09). `Arc` structure also has no
mark bits, so marking traces shared structure once per path:
`append_walk_3200` resolves each live slot 120 to 150 times per collection.
Basic data types should stop using `Arc` (maintainer, 2026-10-09); see
`perf-collection-growth` in the
[structural overheads plan](StructuralOverheads_2026-10-08.md).

The eventual representation should separate:

```text
Public api::Value
└── runtime-local external root
    └── Internal Value word
        ├── immediate scalar/constant
        └── tagged managed pointer
            └── representation-specific managed node
```

## Provisional Invariants

1. **The internal value is cheap to copy.** Target one machine word; permit a
   two-word prototype only if measurements or provenance enforcement justify
   it.
2. **Public values remain a domain-qualified root boundary.** Compact internal
   values do not escape the runtime or replace the public wrapper. A managed
   arm owns one existing collector root cell; an immediate arm has no managed
   allocation to keep live and therefore carries only non-owning domain
   provenance. The distinction remains private to the wrapper.
3. **Immediate values own no Rust resources.** Copying or discarding an
   immediate requires no `Drop`.
4. **Heap representation is split by role.** Large numbers, binaries,
   collections, functions, deferred cells, nets, metadata, and opaque payloads
   use distinct managed representations rather than variants of one large
   allocation enum.
5. **Pointer tags and their alignment are a Glam concern.** The collector
   accepts and returns untagged managed pointers. This plan chooses alignment
   through Rust node representations such as common `repr(align(N))` wrappers,
   removes tags before invoking GC access APIs, and does not ask the heap to
   reinterpret an already-declared type layout.
6. **Run ownership is recoverable.** Given an untagged managed pointer, the
   collector can find its run/page header and static trace metadata without an
   object-local metadata pointer in the compact representation.
   The canonical `&'static ObjectMetadata` address is the operational Rust-type
   identity. A later tagged-pointer cast checks that identity in debug builds;
   release correctness follows from the private tag constructors and managed
   representation invariants.
7. **Moving is not accidentally forbidden.** Trace implementations use an edge
   visitor rather than public offset tables, external roots are enumerable, and
   no public contract promises that a numeric address is permanent identity.
   Rewritable edge slots and relocation are designed only in a future moving-GC
   plan.
8. **The public wrapper is not a semantic key.** It provides transport and may
   provide content-free debugging, but no equality, ordering, or hashing based
   on its root cell, allocation address, tag word, or domain witness. Clients
   first obtain or compute an ordinary host key through runtime-authorized
   semantic observation.
9. **Identity semantics survive movement.** Any use of pointer equality or
   pointer-derived hashing is inventoried before a moving collector is
   attempted.
10. **Observation requires live domain authority.** Structural comparison,
    kind inspection, rendering, evaluation-backed extraction, and all managed
    borrows require a matching live runtime service. An evaluated-value wrapper
    remains the same root plus a WHNF witness rather than a second root or an
    observation capability. Extractors return owned host data when results
    must survive the access scope or value-domain teardown.

## Provisional Encoding Direction

This plan will choose a power-of-two managed-pointer alignment and express it
in the managed Rust node representations. Eight-byte alignment offers three
low zero bits, 16 bytes offers four, and 32 bytes offers five. Typed-run
metadata identifies the concrete managed representation after a word has been
recognized as a pointer, so pointer encodings need not spend low bits
distinguishing every node family. Do not assume that five bits are worth
rounding 16- or 24-byte nodes to 32-byte strides. A later encoded variable-run
alternative would consume part of whichever budget is selected and must
justify changing both the collector and representation plans.
Candidate immediates include:

- signed small integers; small rationals;
- builtin and other compact enumerations;
- unit, empty list, and empty dictionary constants;
- compact atom or intern-table identities; and
- reserved encodings for later use.

The maintainer's direction (2026-10-09) also spends pointer bits on a few
common shapes, so that they need no node of their own or a smaller one:
- any value tagged as a singleton list of itself;
- a few pair types labelled in the pointer, such as list concatenations,
  singleton dicts and rationals; and
- tagged data.

Nodes are therefore expected to shrink well below today's 40-byte list node.
Bootstrap work must not fit caches, such as a cached list length, into that
node's spare bytes (structural overheads plan, `perf-list-leaf-walk`).

Values outside an immediate range become managed objects. In particular:

- small integers are immediate;
- large integers and nontrivial exact rationals use managed number nodes;
- large and shared byte strings use managed or audited immutable leaf storage;
- collection nodes contain compact `Value` words; and
- opaque values use a collector-owned finalizable cell or an explicitly
  external sidecar.

The alignment and tag budget are provisional. Do not assign permanent public
encodings until immediate frequency, node layouts, bitmap overhead, internal
fragmentation, and run-owner lookup have been measured together.

## GC-Facing Run Lookup Decision

The selected initial baseline and one deferred alternative are:

1. **Fixed aligned base runs (initial collector).** Mask the untagged pointer to
   a fixed run boundary; the header supplies slot size and static type metadata.
   Objects which do not fit one supported slot remain unsupported by the
   collector.
2. **Encoded variable run class (deferred alternative).** Reserve part of the
   tagged pointer budget for one of a small number of power-of-two run-size
   classes, then mask according to that class.

A fixed base-run directory can alternatively map every base address to its
typed-run header without encoding run size in every `Value`. It does not imply
multi-run objects. Prefer this if pointer-tag pressure outweighs the extra
directory access.

The initial GC uses fixed aligned runs and must expose an owner-lookup
abstraction, not its header-address formula. It also accepts canonical metadata
whose requested total pre-alignment slot extent may exceed the Rust payload
size without changing the payload's alignment. The extent is not additional
padding and cannot be smaller than the representation. This plan owns both the
Rust node alignment and requested metadata extent. It may compare the
implemented fixed-run lookup with
encoded variable run classes only as a later representation change, after
measuring the abstraction; the GC transition does not reserve tag bits or
multiple run sizes for it.

## Candidate Managed Node Families

The inventory should consider at least:

- big integer and rational nodes;
- binary/text leaves;
- list chunks and concatenation/thunk nodes;
- dictionary index/update nodes;
- partial builtin calls and application/function stages;
- lazy and promise cells;
- metadata cells;
- opaque cells;
- evaluation failure/context nodes; and
- interaction-net values and data wrappers.

Common immutable structural nodes should compare dense 16-, 24-, and 32-byte
layouts under the candidate Rust wrapper-alignment choices. Synchronization-heavy
identities may use larger classes without being treated as representation
failures, but every node must still fit one collector run slot. Variable-sized
or oversized storage remains external or is decomposed; this plan does not
request a collector large-object fallback.

## Transition Phases

### V-1 — Prework from the Pre-Performance Review

These fixes come before V0. Otherwise V0 would measure lock contention, a
wasteful trace, and avoidable per-construction costs rather than the
representation itself. The holistic pre-performance review's findings V1–V3 and V5
are the source.

- **Allocation and root registration without global locks**, now
  `perf-allocation-path` in the structural overheads plan. Each managed
  allocation re-acquires an allocator. That locks a process-wide `TypeId`
  metadata map and the heap's data mutex, and root registration repeats the
  lookup. Use a per-family static or per-thread class cache keyed by
  metadata address, shared by allocators and roots. Compact roots
  opportunistically, and expose `HeapMetrics` through the performance
  harness's counters.
- **A physical, iterative trace.**
  - Walk dictionary values only, and walk each list once, not once for
    thunks and once for values.
  - Drop the per-entry worklist that computes discarded statistics; keep
    those statistics in a test-only variant.
  - Replace value recursion with an explicit worklist.
- **Stack-safe core walks and destruction.**
  - Give `ListNode` an iterative `Drop`.
  - Done in the structural overheads plan's `perf-list-front-walk`: list
    literals are one value leaf, and pops reshape a deep spine toward the
    popped end. A deep spine still drops recursively.
  - Make the key conversions iterative: `Key::to_value_in`,
    `key_from_value` and `value_from_key`.
  - Add small-stack tests that build, trace, collect and drop a 100k-deep
    concatenation and a 10k-nested strict dictionary.
  - The no-unapproved-recursion rule now covers `src/core`; extend it to
    `list.rs`.
- **Per-construction overheads.**
  - Each lazy allocates an `Arc<str>` from a static label.
  - A promise allocates three side `Arc`s.
  - `EvaluationFailure::with_context_in` copies earlier contexts, so k
    frames cost O(k²).
  - Projecting an inline integer allocates a `BigRational`.
  - Atoms, builtins and the cached unit each cost a 64-byte node plus a
    locked root registration.
  - `core::Value` is 64 bytes because `Number(BigRational)` is; boxing
    `Number` is a measurable precursor to V2.

### V0 — Measurements and Semantic Ledger

- Measure `size_of`, alignment, clone/drop behavior, and allocation frequency
  for every current `Value` variant and principal recursive payload.
- Inventory pointer equality, address-derived hashing, unsafe downcasts, and
  representation-sensitive tests.
- Record which small scalar ranges dominate actual samples and tests.
- Build allocation histograms before fixing a slot-size target.
- For candidate 8-, 16-, and 32-byte Rust node alignments, measure tag budget,
  effective metadata-requested stride by node family, slots per run, bitmap
  bytes, and internal fragmentation. Include 24-byte node layouts explicitly.
- Measure collector pressure tuning, which was planned but never measured: the
  survivor ratio (currently one half), run size, and class-cache width.
- Separate logical from physical visits. Tracing is logical: it revisits
  shared persistent spines once per reference. Physical deduplication is
  future work, and needs these counts first.
- Measure two costs the resumable-WHNF work accepted, and revisit them here or
  at concurrent GC's CG0:
  - each published checkpoint quantum takes one mutex and walks every edge
    before and after;
  - exact routes store O(depth) state.
- Consider columnar descriptors for large netlists, which net construction
  deferred to this plan.

### V1 — Isolated Tagged-Word Prototype

- Implement a non-production tagged value module against mock aligned objects.
- Define the private unsafe conversion from a pointer-bearing internal word to
  `Gc<T>`. The value layer owns tag removal and the tag-to-representation
  mapping, then must discharge `glam-gc`'s raw-construction obligations before
  invoking a narrowly exposed, separately audited cross-crate integration
  gateway. That gateway remains outside the supported embedding API.
- In debug builds, compare the mock or real run's canonical metadata pointer
  with the representation selected by the tag. Treat this as an invariant
  diagnostic rather than a release-mode validation policy.
- Prove encode/decode behavior under Miri and property tests.
- Exercise small integers, reserved tags, pointer round trips, and invalid
  encodings.
- Prototype at least the viable 8-, 16-, and 32-byte Rust wrapper-alignment
  choices rather than making the mock allocator silently assume five low tag
  bits.
- Keep arbitrary host and serialized bits outside the live-value decoder.
  Persistence and IPC reconstruct semantic values through validated public
  constructors rather than transmuting stored words into runtime pointers.
- Measure the implemented fixed-run masking path before deciding whether an
  encoded variable run-size class would justify changing both the collector
  geometry and tag budget.

### V2 — Split Scalar and Leaf Representations

- Introduce immediate small integers while preserving exact numeric behavior.
- Move big integers and rationals behind managed nodes.
- Separate binary/text leaf ownership from the general value handle.
- Keep compatibility conversions at module boundaries until consumers migrate.

### V3 — Split Structural Managed Nodes

- Replace the monolithic heap-value enum with representation-specific managed
  nodes.
- Migrate lists and dictionaries without changing their semantic APIs.
- Review the list representation as its own V3 checkpoint. The bootstrap map
  transition preserves each `Concat` lazily but deliberately treats a reached
  `Bytes`, `Values`, or `Finger` representation as one indivisible strict leaf.
  Reconsider that granularity alongside mapped-list nodes and exact-length
  nodes such as `Take n xs`, whose contract would guarantee `n` logical items
  by filling short sources with error values. Such nodes could retain useful
  length information without requiring ordinary thunks to promise a length.
  Do not split large strict leaves or add these nodes merely as part of the
  resumable-WHNF migration.
- Consider lists that are finger trees throughout, with lazy chunks as
  elements measured by known length and count of lazy chunks. Concatenation
  would cost O(log n) rather than O(1), but both ends and index operations
  would stay logarithmic without the observers' reshaping
  (`list-pop-reshapes-remainder`, and the structural overheads plan's
  `perf-list-leaf-walk`). Until then a program that indexes one list
  repeatedly asks for it with the `array` or `deque` annotation.
- Migrate functions, partial calls, failures, metadata, and deferred values in
  independently testable checkpoints.

### V4 — Runtime and Public-Root Integration

- Instantiate the managed node wrappers and canonical metadata policy selected
  by V0/V1 before allocating values. A canonical Rust type retains one layout
  and requested slot size; use another wrapper type rather than changing the
  policy of an existing metadata identity.
- Make the opaque public wrapper contain either a domain-qualified immediate
  internal value or the existing collector root cell for a managed node.
  Managed root type erasure, if required by the split node taxonomy, must reuse
  that root cell rather than add another registry or root representation.
- Replace V1's mock validation with real heap/run ownership, live-slot, and
  canonical-metadata assertions at the private decoding boundary. Public value
  APIs must provide no route to forge or reinterpret pointer-bearing words.
- Update evaluator, reflection, event, diagnostic, and task boundaries.
- Preserve cheap cross-thread public-root cloning and runtime provenance
  checks.

### V5 — Remove Transitional Ownership

- Remove redundant `Arc` and enum wrappers.
- Remove conversions which can no longer be reached.
- Remove `RuntimeFailureRoot`'s parallel `Arc<EvaluationFailure>` plus direct
  value-root representation after failures have one canonical managed/rooted
  form.
- Remove every remaining semantic-data-plus-parallel-roots record and every
  `CompatibilityValueEdges` implementation. A source-backed gate must prove
  that poll-spanning machine state uses canonical roots, specialized managed
  roots, or edge-free phase data before moving collection may begin.
- Re-run layout and allocation measurements and tune size classes only from
  evidence.
- Update architecture and public API documentation.

## Verification

Each phase compares old and new representations over:

- all syntax and assembly samples;
- exact arithmetic, including values crossing the immediate boundary;
- equality and dictionary-key behavior;
- lazy, promise, metadata, function, and collection cycles;
- public root cloning and cross-runtime rejection;
- reflection inspection and diagnostic rendering;
- each selected/candidate Rust alignment and metadata size policy's pointer
  decoding, slot geometry, and semantic equivalence;
- forced full-collection histories; and
- Miri, deterministic concurrency tests, and the standard repository checks.

Differential tests should evaluate the same constructed values through both
representations until the old form is retired.

## Deferred With This Plan

- a moving collector implementation;
- permanent serialized encodings of live values;
- pointer compression beyond low-bit tagging;
- NaN boxing;
- JIT stack maps;
- compact UTF-8 or short-binary immediates; and
- changing dictionary or list semantics merely to meet a slot-size target.
