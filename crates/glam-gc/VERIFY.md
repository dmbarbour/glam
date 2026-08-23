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
class frontier is exhausted; after a claim, a worker is the only writer of its
atomic allocation word. C2C.6's native barrier fixtures force eight production claimers
past the same exhausted-frontier observation and verify one synchronized
advance or publication plus seven winner-frontier rechecks. C3 adds Loom models
for mutator-exit visibility, unique idle-entry election, reciprocal nested
admission with requests latched, and the no-gap
exclusive-to-finalizer-to-entry handoff. Native forced schedules exercise
production request epochs, idle-entry and synchronous election, waiter
coalescing, direct admission transfer, collector-local cache reset, the absence
of exit-time service, the finalizer mutator, request/pressure acknowledgement,
and unwind restoration. The collection body remains synthetic: C4 through C6
separately own roots, exact tracing, reclamation, and destructor recovery. C4A
adds release-checked direct roots and makes allocation-bit publication atomic
so root validation can inspect a word while its leased writer advances other
slots. C4B publishes each cell into a weak heap registry before returning its
public root and adds exclusive, stable, in-place traversal and pruning. C4C
integrates that walk with every elected collection and forces the last-root
drop on both sides of a temporary weak upgrade. The C4 walk remains a no-op
seed receiver until C5 adds marking.
Ordinary threaded stress remains supplementary rather than proof of coordinator
ordering.

## C5 Exact-Mark Verification

The ordinary stable check compares 24 deterministic randomized managed graphs
with an independent index-based reachability oracle. It checks the successful
report, every allocation's mark bit, and each object's trace count. A separate
full-run fixture drives one assigned run through zero, one, all, and zero live
slots across four successful collections, proving that each attempt clears the
prior bitmap before publishing new reachability. That roughly eight-thousand-
root fixture is native-only; Miri retains the existing focused bitmap boundary,
partial-mark recovery, and one bounded 65-node randomized-oracle case.

The native million-edge fixtures are intentionally isolated from routine unit
test latency. Run them serially, with their worklist measurement visible, via:

```sh
crates/glam-gc/scripts/check-scale.sh
```

That script exercises a one-million-node chain and a flat one-million-edge
array through the checked non-recursive production marker. On 2026-08-23 the
flat fixture reported a peak object-worklist length of 1,000,000 and capacity
of 1,048,576. These are observations of the current LIFO `Vec` worklist, not
correctness or performance thresholds. The routine native suite retains its
20,000-node chain, while Miri uses the same path with 256 nodes; stack-depth and
million-edge scale remain native responsibilities.

C5D.2 adds no unsafe operation or module opt-in. The exact unsafe inventory
must therefore remain unchanged while these fixtures and their ledger entries
are added.

ThreadSanitizer passes the complete C5D.2 native suite. The post-C5 review
isolated the prior 24-byte LeakSanitizer report to
`forgotten_scoped_allocator_does_not_retain_its_heap`, the C4D fixture which
deliberately calls `mem::forget` on one inert frontier cell to prove that an
escaped allocator does not retain its heap. It is not TLS or managed-heap
retention and reproduces from the clean pre-C5D.2 `d7977d4` worktree.
`check-sanitizer.sh address` therefore runs every other test with leak
detection enabled, then runs that exact ownership fixture with ASan enabled and
leak detection disabled. This is an explicit process-lifetime-fixture
exception, not a general LeakSanitizer suppression.

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
