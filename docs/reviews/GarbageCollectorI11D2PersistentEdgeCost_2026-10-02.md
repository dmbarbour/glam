# Garbage Collector I11D.2e Persistent-Edge Cost Closure — 2026-10-02

Baseline: `756073ab`, after the I11D.2b-I11D.2d dynamic-tool matrix.

Status: complete. The explicit persistent-edge API retains one-pointer layout,
creates no managed allocation or registered root when duplicated or compared,
and lowers in an optimized build to the minimum pointer operations. No lock,
reference-count, allocation, root-registration, or other hidden release cost
was found.

## Scope

I11D.2e closes P5B of
[`GarbageCollectorPersistentEdgeTraits_2026-09-12.md`](../plans/GarbageCollectorPersistentEdgeTraits_2026-09-12.md).
It does not reopen the completed ownership migration or establish a portable
wall-clock performance threshold. The question is narrower: did replacing
implicit `Gc<T>` copying and equality with `duplicate_in` and
`same_allocation_in` accidentally put runtime bookkeeping on every ordinary
managed edge operation?

## Layout and Traffic Evidence

`Gc<T>` remains `repr(transparent)` over one `NonNull<T>`. The unconditional
const assertion in `pointer.rs` requires
`size_of::<Gc<u64>>() == size_of::<*const u64>()` in every build.

The existing root fixtures were rerun and remain authoritative:

- `pointer_duplication_and_identity_register_no_roots` observes zero root
  registrations before and after duplication and identity comparison;
- `root_registry_publishes_once_per_cell_and_not_per_clone` proves cloning a
  root handle does not publish another root cell; and
- `root_projection_preserves_identity_without_registering_another_root`
  proves projecting a root back to `Gc<T>` does not register another root.

The new deterministic fixture
`pointer_duplication_and_identity_allocate_no_managed_slots` takes exact
allocated-slot and monotonic root-registration snapshots, performs 1,024
duplicates plus same/distinct identity observations, and requires both counts
to remain unchanged. It therefore distinguishes a cheap persistent edge from
a temporary managed allocation or root even if that temporary owner would be
dropped before the assertion.

## Release Code Generation

[`persistent_edge_codegen.rs`](../../crates/glam-gc/examples/persistent_edge_codegen.rs)
provides two policy-free, non-inlined wrappers. The stable verification script
compiles them with the release profile and inspects optimized LLVM IR:

```sh
crates/glam-gc/scripts/check-persistent-edge-codegen.sh
```

On stable Rust 1.98.1 with LLVM 22.1.8, `duplicate_in` lowers to:

```llvm
%edge = load ptr, ptr %value
ret ptr %edge
```

`same_allocation_in` lowers to:

```llvm
%left_edge = load ptr, ptr %left
%right_edge = load ptr, ptr %right
%same = icmp eq ptr %left_edge, %right_edge
ret i1 %same
```

The mutator argument is absent from both optimized bodies. The script requires
these exact instruction shapes and independently rejects calls, stack
allocation, atomic read-modify-write, compare-exchange, fences, and invokes.
It therefore fails if a future implementation adds a root, lock, reference
count, heap allocation, or other release bookkeeping, even if functional tests
continue to pass.

The collector's ordinary `check.sh` now invokes this release check after its
all-feature test suite, making P5B a continuing regression boundary rather
than a one-time visual inspection.

## Source-Ledger Reconciliation

The fixture adds five deliberately test-only typed-edge occurrences: the two
code-generation wrapper calls and three operations in the allocation/root
traffic latch. The exact persistent-edge inventory advances from 873 to 878
occurrences and from 675 to 680 test typed occurrences. Production remains
exactly 148 typed and 36 erased occurrences; test erased identity remains 14.
The reviewed source-qualified fingerprint is now
`7_836_447_809_315_229_698`, with no defect or pending classification.

The first complete workspace run rejected the old count and partition before
running to completion. The mismatch was reproduced through both focused
inventory tests, attributed to these five exact occurrences, and only then
accepted into the count, partition, commentary, and fingerprint. A passing
rerun therefore verifies the reviewed change rather than treating inventory
drift as an automatically updated baseline.

## Verification

The completed boundary passes:

```text
crates/glam-gc/scripts/check.sh
    204 unit tests passed, 2 scale fixtures ignored
    7 Loom models passed
    8 compile-fail/doc tests passed
    persistent-edge release code-generation check passed
cargo fmt --check
    passed
cargo clippy --all-targets --all-features -- -D warnings
    passed
cargo test -q
    root library: 1,882 passed, 2 ignored
    every auxiliary target passed
scripts/check-interaction-net-profiling.sh
    passed
```

This checkpoint changes tests, a release-inspection example, verification
scripts, exact test-only inventory records, and documentation. It changes no
collector/runtime implementation, unsafe boundary, semantic edge, root or
admission behavior, finalization path, or scheduler policy. The completed
repository-aggressive workspace result therefore remains valid under I11D's
explicit rerun rule; the new deterministic collector test ran through the
all-feature collector check.

## Disposition

P5B and I11D.2e are complete. The explicit API has the intended semantic
friction at source level without release-mode runtime friction. Follow-up:
I11D.3 completed the static source-delta, ledger, protocol, and focused closure
audits on 2026-10-02; I11D.4 subsequently certified Gate G3 that day.
