# Collector Verification

Run the stable collector checks from the repository root:

```sh
crates/glam-gc/scripts/check.sh
```

The script formats, lints, and tests the crate, including compile-fail doctests
and the Loom models. `audit-unsafe.sh` then compares every unsafe construct
and every module-level unsafe opt-in with the checked-in inventories before
building all crate targets and features under the crate's default
`unsafe_code` denial.

Optional toolchain checks:

```sh
crates/glam-gc/scripts/check-miri.sh
crates/glam-gc/scripts/check-sanitizer.sh address
crates/glam-gc/scripts/check-sanitizer.sh thread
```

The sanitizer scripts exercise the collector library rather than the separate
Loom scaffold. Even an empty `loom::model(|| {})` currently retains one
256-byte Loom allocation under LeakSanitizer and emits the documented ASan
stack-switch warning; that tool incompatibility is independent of Glam heap
entry and explicit TLS release. The ordinary stable check continues to run the
Loom models separately.

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

The Loom tests retain the heap-entry tooling/API smoke model and model C2C.5's
atomic lease-bit claim transition. Raw arena-pointer integration remains under
native forced schedules, sanitizers, and Miri. C2C.5 claims whole allocation
words through atomic lease bitmaps and consults the heap mutex only when a
class frontier is exhausted; after a claim, a worker mutates only its own
ordinary word. C2C.6's native barrier fixtures force eight production claimers
past the same exhausted-frontier observation and verify one synchronized
advance or publication plus seven winner-frontier rechecks. C3 adds Loom models
for mutator-exit visibility, pending-exclusive priority, reciprocal dependent
admission, and the no-gap exclusive-to-finalizer handoff. Native forced
schedules exercise production request epochs, election, waiter coalescing, TLS
admission classification, deferred service, the finalizer mutator, follow-up
commitment, and unwind restoration. The collection body remains synthetic: C4
through C6 separately own roots, exact tracing, reclamation, and destructor
recovery. Ordinary threaded stress remains supplementary rather than proof of
coordinator ordering.

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
