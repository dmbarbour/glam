# Value Representation Refinement Plan — 2026-08-19

Status: preliminary and deliberately deferred until the initial Glam-owned GC
boundary is working. This plan must not expand the collector implementation or
integration gates merely to obtain a compact value representation.

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

## Current Pressure

The current enum stores representations such as `Number(BigRational)` inline.
Its size is therefore determined by its largest variant rather than by the
common atom, builtin, small integer, or managed-pointer cases. It also combines
semantic value classification with Rust ownership glue such as `Arc`, making
trivial reclamation and compact copying difficult.

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
8. **Identity semantics survive movement.** Any use of pointer equality or
   pointer-derived hashing is inventoried before a moving collector is
   attempted.

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
