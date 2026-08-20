# Value Representation Refinement Plan — 2026-08-19

Status: preliminary and deliberately deferred until the initial Glam-owned GC
boundary is working. This plan must not expand the collector implementation or
integration gates merely to obtain a compact value representation.

## Purpose

Replace the current large Rust `core::Value` enum with a compact internal value
handle after runtime-local tracing ownership is established. The intended
direction is a tagged immediate-or-managed-pointer word, with common managed
nodes occupying approximately 32-byte slots and exceptional values using
larger allocation classes.

This is a performance and representation transition. It must preserve Glam
evaluation, identity, confluence, reflection, diagnostics, and public API
semantics.

## Why This Is Separate From GC

The collector needs to know how to:

- enumerate managed edges through a visitor without freezing a public offset
  representation;
- find allocation/run metadata from an untagged managed pointer;
- provide a documented minimum managed-pointer alignment; and
- root values at the Rust API boundary.

It does not need to know Glam's immediate tags, numeric encoding, builtin
encoding, or semantic node taxonomy. Conversely, this plan may rely on the
collector's public alignment and owner-lookup contract, but must not reach into
collector-private page tables or bitmap representations.

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
2. **Public values remain roots.** Compact internal values do not escape the
   runtime or replace the public root boundary.
3. **Immediate values own no Rust resources.** Copying or discarding an
   immediate requires no `Drop`.
4. **Heap representation is split by role.** Large numbers, binaries,
   collections, functions, deferred cells, nets, metadata, and opaque payloads
   use distinct managed representations rather than variants of one large
   allocation enum.
5. **Pointer tags are a Glam concern.** The collector accepts and returns
   untagged aligned managed pointers. Glam removes tags before invoking GC
   access APIs.
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

Assuming at least 32-byte managed-pointer alignment, five low bits are
available for the combined pointer-run-class and Glam immediate tag scheme.
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

The five-bit budget is provisional. Do not assign permanent public encodings
until the run-owner lookup and immediate inventory are measured together.

## GC-Facing Run Lookup Decision

Two compatible owner-lookup shapes remain under consideration:

1. **Fixed aligned base runs.** Mask the untagged pointer to a fixed run
   boundary; the header supplies slot size and static type metadata. Objects
   which do not fit one supported slot remain unsupported by the collector.
2. **Encoded variable run class.** Reserve part of the tagged pointer budget for
   one of a small number of power-of-two run-size classes, then mask according
   to that class.

A fixed base-run directory can alternatively map every base address to its
typed-run header without encoding run size in every `Value`. It does not imply
multi-run objects. Prefer this if pointer-tag pressure outweighs the extra
directory access.

The initial GC must expose an owner-lookup abstraction and minimum alignment,
not its header-address formula. This plan selects the concrete encoding only
after measuring that abstraction.

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

Common immutable structural nodes should target a 32-byte allocation class.
Synchronization-heavy identities may use larger classes without being treated
as representation failures, but every node must still fit one collector run
slot. Variable-sized or oversized storage remains external or is decomposed;
this plan does not request a collector large-object fallback.

## Transition Phases

### V0 — Measurements and Semantic Ledger

- Measure `size_of`, alignment, clone/drop behavior, and allocation frequency
  for every current `Value` variant and principal recursive payload.
- Inventory pointer equality, address-derived hashing, unsafe downcasts, and
  representation-sensitive tests.
- Record which small scalar ranges dominate actual samples and tests.
- Build allocation histograms before fixing a slot-size target.

### V1 — Isolated Tagged-Word Prototype

- Implement a non-production tagged value module against mock aligned objects.
- Define the private unsafe conversion from a pointer-bearing internal word to
  `Gc<T>`. The value layer owns tag removal and the tag-to-representation
  mapping, then must discharge `glam-gc`'s raw-construction obligations before
  invoking its constructor.
- In debug builds, compare the mock or real run's canonical metadata pointer
  with the representation selected by the tag. Treat this as an invariant
  diagnostic rather than a release-mode validation policy.
- Prove encode/decode behavior under Miri and property tests.
- Exercise small integers, reserved tags, pointer round trips, and invalid
  encodings.
- Keep arbitrary host and serialized bits outside the live-value decoder.
  Persistence and IPC reconstruct semantic values through validated public
  constructors rather than transmuting stored words into runtime pointers.
- Compare fixed-run masking with an encoded run-size class before selecting the
  tag budget.

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

- Make runtime roots contain the compact internal value.
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
- forced full and minor collection histories; and
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
