# Collector Verification

Run the stable C0 checks from the repository root:

```sh
crates/glam-gc/scripts/check.sh
```

The script formats, lints, tests the crate (including the Loom smoke model), and
forbids unsafe code to latch the C0 baseline. Once reviewed unsafe code is
introduced, `audit-unsafe.sh` must be replaced by an auditable unsafe-surface
report rather than silently removed.

Optional toolchain checks:

```sh
crates/glam-gc/scripts/check-miri.sh
crates/glam-gc/scripts/check-sanitizer.sh address
crates/glam-gc/scripts/check-sanitizer.sh thread
```

These scripts deliberately fail with the underlying toolchain diagnostic when
nightly, Miri, `rust-src`, or a sanitizer is unavailable. C0 does not claim that
every developer environment has installed those tools.

The Loom test is currently a tooling and API smoke model. C0 has no shared
collector state. A later phase must replace or supplement it with models of
each real coordination state machine; ordinary threaded stress is not a proof.

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
