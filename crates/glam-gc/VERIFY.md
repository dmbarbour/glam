# Collector Verification

Run the stable collector checks from the repository root:

```sh
crates/glam-gc/scripts/check.sh
```

The script formats, lints, and tests the crate, including compile-fail doctests
and the Loom smoke model. `audit-unsafe.sh` then compares every unsafe construct
and every module-level unsafe opt-in with the checked-in inventories before
building all crate targets and features under the crate's default
`unsafe_code` denial.

Optional toolchain checks:

```sh
crates/glam-gc/scripts/check-miri.sh
crates/glam-gc/scripts/check-sanitizer.sh address
crates/glam-gc/scripts/check-sanitizer.sh thread
```

These scripts deliberately fail with the underlying toolchain diagnostic when
nightly, Miri, `rust-src`, or a sanitizer is unavailable. Every checkpoint
which changes the unsafe allocation or access surface records a focused Miri
pass; later routine runs remain optional when the local toolchain component is
unavailable.

C1 temporarily passed `-Zmiri-ignore-leaks` because its prototype allocator
used `Box::leak`. C2C.1b replaced that path with arena-owned allocation and
terminal payload destruction, so `check-miri.sh` now keeps leak checking
enabled alongside pointer provenance, aliasing, initialization, thread access,
and mismatch gateways.

The Loom test remains a tooling and API smoke model. C2C's slow path leases
whole allocation words under the heap mutex, after which a worker mutates only
its own word; there is still no collection state machine to model. C3 must
replace or supplement this smoke test with models of each real admission and
stop-the-world transition. C2C's forced native schedules and Miri validate the
current disjoint-word implementation, but ordinary threaded stress alone is
not a proof of the later coordinator.

## Gate G0 Baseline

Before changing the unsafe surface in C1, recheck the focused pre-GC semantic
contracts with:

```sh
crates/glam-gc/scripts/check-g0-semantics.sh
```

The operational comparison data can be recaptured on Linux with:

```sh
crates/glam-gc/scripts/capture-g0-baseline.sh
```

That script reports release-process timing and peak RSS; it does not enforce
performance thresholds. The dated measurements, environment, methodology, and
known pre-GC worker-stack observation are recorded in
[`GarbageCollectionGateG0Baseline_2026-08-20.md`](../../docs/plans/GarbageCollectionGateG0Baseline_2026-08-20.md).
