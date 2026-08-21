# Collector Verification

Run the stable C1 checks from the repository root:

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
nightly, Miri, `rust-src`, or a sanitizer is unavailable. C1 completion
requires one recorded focused Miri pass; later routine runs remain optional
when the local toolchain component is unavailable.

During C1 only, `check-miri.sh` passes `-Zmiri-ignore-leaks` because the
prototype allocator deliberately uses `Box::leak`. Miri still checks pointer
provenance, aliasing, initialization, thread access, and the mismatch gateways.
C2 must remove this exception when arena-owned allocation replaces the
prototype; it is not permission for the collector to leak.

The Loom test remains a tooling and API smoke model. C1A's debug allocation
registry is not collector coordination, and the leaking prototype has no
collection state machine to model. C3 must replace or supplement this with
models of each real admission and stop-the-world transition; ordinary threaded
stress is not a proof.

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
