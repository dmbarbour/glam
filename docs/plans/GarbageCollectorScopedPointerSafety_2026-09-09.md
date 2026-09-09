# Garbage-Collector Scoped Pointer Safety Plan — 2026-09-09

Status: tentative safety-enhancement plan. This is not part of GCI5R-008, the
initial non-moving collector integration, or its Gate G2-G4 requirements. Do
not begin this transition until the existing integration has stabilized and a
fresh review coordinates it with Value Representation Refinement and any
moving-collector work.

## Purpose

Make the distinction between a persistent managed edge and a pointer currently
authorized for use by one mutator region explicit in Rust's types.

The current `Gc<T>` is a pointer-sized, `Copy` and `Clone`, non-rooting handle.
It cannot be safely dereferenced without a matching mutator, but it can be
copied into arbitrary Rust state without recording whether that state is a
traced edge, a temporary working value, or an accidental escape. This is
adequate for the initial specialized non-moving collector, but provides little
structural resistance to stale pointers during later representation changes.

The tentative direction inverts where cheap copying is available:

```rust
Gc<T>                  // persistent stored edge; move-only
ScopedGc<'mutator, T>  // temporary admitted view; Copy and Clone
```

`Gc<T>` remains pointer-sized and non-rooting, but loses `Copy` and `Clone`.
Reading an edge from a managed object or root under a matching mutator produces
a pointer-sized `ScopedGc<'mutator, T>`. Only that scoped form supports cheap
working copies, dereference, and ordinary pointer-identity observations.

This is intended as compile-time resistance and audit structure, not linear
ownership of an allocation. GC reachability still comes only from registered
roots and exact tracing.

## Relationship to Existing Plans

- [GarbageCollectorImplementation_2026-08-19.md](GarbageCollectorImplementation_2026-08-19.md)
  deliberately makes the initial `Gc<T>` cheap to copy. This plan revisits
  that decision only after the initial collector is working end to end.
- [GarbageCollectorIntegration_2026-08-19.md](GarbageCollectorIntegration_2026-08-19.md)
  must finish with its current pointer representation. No current integration
  checkpoint should be enlarged to prototype this model.
- [ValueRepresentationRefinement_2026-08-19.md](ValueRepresentationRefinement_2026-08-19.md)
  will substantially change where managed edges reside and how internal values
  are copied. A full scoped-pointer migration should normally follow or be
  combined with that transition rather than migrating today's large
  compatibility representation twice.
- [ConcurrentGarbageCollection_2026-08-28.md](ConcurrentGarbageCollection_2026-08-28.md)
  remains non-moving, but its mutator epochs and mutation barriers provide the
  likely coordination vocabulary for scoped pointer use.
- A future moving-collector plan must decide how stored edges are rewritten.
  This plan prevents temporary read pointers from silently acquiring a
  permanent-address contract, but does not itself implement relocation.

GCI5R-008 may add a mutator-qualified `Root::as_gc` returning the current
unbranded `Gc<T>`. That is a local API repair and an admitted-use convention,
not lifetime enforcement. This later plan may change its result to
`ScopedGc<'mutator, T>` without restoring cached root/edge pairs.

## Provisional Model

### Persistent stored edges

`Gc<T>` represents one pointer stored in a location whose liveness is justified
elsewhere. It is:

- pointer-sized;
- non-rooting;
- movable between Rust owners;
- `Send` and `Sync` when the represented type permits it;
- neither `Copy` nor `Clone`; and
- not directly dereferenceable or comparable by address.

Moving the handle does not alter the managed graph. Creating another persistent
handle is an explicit operation performed under mutator or mutation-gateway
authority. This makes durable duplication searchable and reviewable.

Making the handle move-only does not prove that its Rust storage is traced.
An unrooted `Gc<T>` can still be moved into an untraced external object and
become stale. Existing regional-allocation, first-owner, opaque-boundary, and
trace-completeness rules remain necessary.

### Scoped working views

The provisional access shape is:

```rust
impl<T: Trace> Gc<T> {
    fn access<'mutator>(
        &self,
        mutator: &'mutator Mutator<'_>,
    ) -> ScopedGc<'mutator, T>;
}

impl<T: Trace> Root<T> {
    fn access<'mutator>(
        &self,
        mutator: &'mutator Mutator<'_>,
    ) -> ScopedGc<'mutator, T>;
}
```

The exact lifetime relationship must be prototyped; the sketch expresses the
intention rather than final Rust syntax. `ScopedGc` is branded by the admitted
mutator borrow, is `Copy` and `Clone`, and should be non-`Send` and non-`Sync`
through the mutator brand. It may:

- borrow `T` safely for no longer than the mutator region;
- perform pointer-identity comparisons against another compatible scoped view;
- be copied through local algorithms and worklists; and
- supply the typed pointer to collector-private mutation and trace machinery.

No scoped reference or view may cross a wait, callback, mutator exit, or
safepoint which permits relocation or reclamation.

### Persisting another edge

Some operations genuinely need to duplicate a semantic edge—for example,
cloning a Glam value into another exactly traced field. The scoped view must
therefore support an explicit conversion back into a persistent `Gc<T>`.
Possible final spellings include:

```rust
mutator.persist_edge(scoped)
edge_writer.install(scoped)
scoped.to_stored(mutator)
```

Because `ScopedGc` is copyable, this conversion cannot enforce unique
ownership: a caller can deliberately persist it more than once. Its value is
that every persistent duplication crosses a named, searchable boundary. The
preferred production form should associate conversion with the traced
destination or mutation gateway where practical, allowing later SATB,
generational, or relocation barriers to share that boundary.

Allocation returns one fresh persistent edge or a distinct fresh-edge token.
The transition must compare both designs. A fresh token may better express
first-owner publication, but it is not justified if it adds wrapper churn
without preventing an observed defect.

### Tracing and rewriting

The current `Trace` visitor consumes copied `Gc<T>` values. A move-only stored
edge likely changes ordinary tracing to borrow the edge:

```rust
visitor.visit(&self.child);
```

That is sufficient for non-moving marking. Moving collection additionally
needs an exact mutable edge slot or stable indirection so it can rewrite every
persistent pointer. This plan must not claim moving readiness merely because
temporary views are lifetime-branded.

Registered roots are different from stored graph edges: the collector owns
their root cells and can update their allocation pointer while mutators are
stopped. `Root::access` then projects the current pointer into the newly
admitted mutator epoch.

## Expected Benefits

1. Managed dereference becomes safe only on a mutator-branded type.
2. Temporary algorithmic pointer copies cannot escape their mutator lifetime
   through safe Rust.
3. Persistent edge duplication becomes explicit and source-inventoriable.
4. Root projection naturally observes the allocation address current for its
   mutator epoch rather than encouraging a cached sibling `Gc<T>`.
5. A later moving collector has a clear distinction between rewritable stored
   slots and disposable scoped views.
6. Mutation barriers gain one structural gateway through which a scoped view
   becomes durable graph state.

## Costs and Limitations

1. This is a broad source migration. `Value`, collections, lazy sources,
   failures, functions, interaction nets, and compatibility adapters currently
   rely on cheap `Clone` implementations which transitively copy `Gc<T>`.
2. Every stored-edge read must receive or derive mutator authority. Applying
   this only to roots would create ceremony without closing the other escape
   paths.
3. A copyable `ScopedGc` can be persisted repeatedly, so the model is not an
   affine proof and does not replace mutation barriers or trace audits.
4. A move-only `Gc<T>` still does not prove that its destination is managed,
   rooted, or traced.
5. Borrowed tracing alone does not enable relocation; mutable edge discovery
   or stable handles remain necessary.
6. Rust lifetime and variance details may make a pleasant public spelling more
   difficult than the conceptual model suggests. The isolated prototype must
   demonstrate ergonomics before production migration.
7. Thread-local worklists which currently store copied pointers may need to
   store scoped views, stable IDs, or explicitly persisted edges according to
   their lifetime.

No runtime performance win is assumed. The intended scoped type should be
pointer-sized and compile away, but additional heap checks, mutator threading,
or persistent-edge gateway calls must be measured.

## Alternatives to Compare

1. **Full inverted model (preferred experiment).** Move-only stored `Gc<T>`
   plus copyable `ScopedGc<'mutator, T>`, with explicit persistence.
2. **Branded dereference only.** Keep `Gc<T>: Copy`, but permit safe access
   only through a scoped view. This is much cheaper to migrate but does not
   inventory persistent duplication.
3. **Stable handle or slot identity.** Make `Gc<T>` copyable because it names a
   stable indirection rather than an allocation address. This eases moving GC
   but adds lookup, storage, and cache costs to ordinary access.
4. **Current unsafe internal boundary.** Retain copyable raw pointers and rely
   on private constructors, mutator-qualified access, exact tracing, and
   review. This remains the baseline unless the prototype demonstrates a
   worthwhile safety improvement.

## Transition Phases

### SP0 — Fresh Inventory and Decision Gate

- Re-audit every `Gc<T>` field, clone, equality comparison, trace call,
  worklist entry, channel crossing, root projection, and mutation gateway after
  GC integration and Value Representation Refinement reach their actual state.
- Classify each occurrence as persistent traced edge, fresh unpublished edge,
  registered root projection, scoped working copy, external invalid escape, or
  collector-private identity.
- Reproduce at least one concrete defect or high-risk construction which the
  inverted model rejects at compile time. Do not proceed solely because the
  type model appears aesthetically cleaner.
- Compare the four alternatives above and select explicit performance and
  safety success criteria.

### SP1 — Isolated `glam-gc` Prototype

- Prototype move-only `Gc<T>` and pointer-sized `ScopedGc<'mutator, T>` in an
  isolated module or feature, without changing Glam production types.
- Establish variance, `Send`/`Sync`, `Copy`/`Clone`, mutator branding,
  dereference, equality, root projection, fresh allocation, and explicit
  persistence behavior.
- Add compile-fail tests for scoped-view escape, cross-thread transfer,
  access without a mutator, and implicit persistent cloning.
- Run layout assertions, Miri, Loom where synchronization is involved, and
  microbenchmarks for access and local pointer-copy loops.
- Stop if the design needs allocation, reference counting, pointer-local
  locking, or a second machine word merely to express the brand.

### SP2 — Trace and Mutation Protocol Prototype

- Change prototype tracing to borrow persistent edges and prove complete
  non-recursive marking without copying them.
- Prototype explicit persistence through both a plain mutator and an
  owner-qualified mutation gateway. Decide which is the minimum public
  collector API and which is Glam integration policy.
- Determine the future relocation seam: mutable slot visitor, collector-owned
  edge cell, or stable handle. Record it without implementing moving GC.
- Verify first-owner publication, leaving-edge SATB capture, and ordinary STW
  behavior remain expressible without unbranded temporary pointers.

### SP3 — Glam Representation Migration Plan

- Only after SP0-SP2 succeed, produce a separate implementation plan for Glam.
- Coordinate that plan with the then-current compact `Value` representation so
  cloning semantic values becomes a mutator-authorized operation rather than
  a mechanical derive.
- Partition by values and persistent containers, deferred identities and
  failures, interaction nets, scheduler work, public roots, and compatibility
  removal. Avoid one atomic whole-repository cutover unless compilation makes
  that unavoidable.
- Preserve the established no-mutator-across-wait/callback boundary and use
  deterministic ordering fixtures rather than repeated concurrency tests.

### SP4 — Closure and Moving-GC Handoff

- Remove unbranded dereference and implicit stored-edge duplication APIs.
- Re-run exact source inventories for raw pointers, roots, barriers, and
  external owners.
- Measure layouts, cloning cost, allocation throughput, evaluator throughput,
  and trace cost against the pre-transition baseline.
- Update the future moving-GC plan with the selected root-update and persistent
  edge-rewrite protocol. Lifetime branding alone is not a moving-GC gate.

## Verification Requirements

- Compile-fail evidence demonstrates the intended rejected programs; prose and
  source scans are not enough.
- Miri covers pointer reconstruction, scoped dereference, root projection, and
  persistence boundaries under strict provenance.
- Deterministic fixtures force root publication, edge replacement, collection,
  and mutator-exit orderings. Repetition is not evidence for a concurrency
  claim.
- Differential tests compare reachability, cycle reclamation, evaluation,
  diagnostics, and interaction-net behavior with the existing representation.
- `size_of::<Gc<T>>()` and `size_of::<ScopedGc<'_, T>>()` remain one pointer
  unless a separately reviewed measurement justifies otherwise.
- Routine formatting, Clippy, and full test checks pass at every production
  checkpoint.

## Deferred Decisions

- Final names (`ScopedGc`, `GcRef`, `GcAccess`, or another spelling).
- Whether allocation returns `Gc<T>` directly or a distinct fresh-edge token.
- Whether persistent conversion is a general mutator operation or exists only
  through destination-aware edge writers in Glam production code.
- The exact mutable-edge representation used by a moving collector.
- Whether the transition follows Value Representation Refinement or becomes
  one of its implementation phases after a future joint review.
