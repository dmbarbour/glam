# Glam GC Gate G3 Certification — 2026-10-02

Gate G2 baseline: `585cfec3`, certified on 2026-09-11.

Complete ordinary/aggressive ownership baseline: `5759ee6d`, certified on
2026-10-01 after D.2h.

Final static-audit baseline: `2e19bb29`, after I11D.3 on 2026-10-02.

Status: **Gate G3 passed.** I11's complete production graph, controlled serial
collection, worker/finalizer schedules, repository-aggressive mode, dynamic
unsafe-boundary verification, persistent-edge cost boundary, and final static
audit agree. This authorizes I12's explicit runtime-maintenance design work; it
does not enable automatic collection or change any existing heap from
`CollectionPolicy::NoAuto`.

## Certification Scope

Gate G3 asks whether the initial non-moving stop-the-world collector may safely
collect the complete production runtime value graph at an explicitly selected
maintenance boundary. It does not certify concurrent or moving collection,
automatic collection heuristics, arbitrary callback-time maintenance, or a
public collection API.

The certification composes these independently reviewed results:

| Evidence | Certified boundary |
| --- | --- |
| Gate G2 and I11A | every managed family, trace edge, durable root, mutation gateway, external-owner exception, and requested layout has a closed source/fixture record |
| I11B | private serial production collection preserves compilation, reflection, diagnostics, events, settlement, readiness stamps, and net topology; closed managed cycles reclaim while reviewed external owners retain then retire explicitly |
| I11C plus I11D.0 | worker exclusion, passive production finalization, request coalescing during finalization, and runtime retirement use deterministically observed schedules and exact allocation accounting |
| I11D.1 / D.2h | the complete ordinary and repository-aggressive workspaces pass; regional ownership gaps and invalid fixture handoffs are repaired rather than skipped |
| I11D.2 | focused Miri, ASan/LSan, and TSan matrices pass their supported targets; persistent-edge duplication/identity remains one-pointer, allocation-free, root-free, and call-free in release code |
| I11D.3 | the final unsafe, trace, mutation, admission, lock/wait, finalizer, external-owner, and authoritative-ledger delta is closed without a production repair |

No one row substitutes for another. Source inventories identify the current
surface, forced fixtures establish semantic/order claims, dynamic tools inspect
unsafe execution, and complete workspaces close integration behavior.

## Collection Modes and Rerun Decision

Production heap policy remains immutable `NoAuto` in both verified repository
configurations:

- **ordinary mode** collects only through the private controlled maintenance
  seam used by I11 fixtures;
- **repository-aggressive verification mode** requests collection before every
  eligible outer entry after a complete production runtime is constructed. It
  is a private compile-time test feature, not a supported embedding policy.

The full aggressive workspace passed at `5759ee6d`. From that baseline through
I11D.3, the complete Rust/manifest/script delta is seven verification-only
files: a release code-generation example and script, script registration, one
Miri-only scale reduction, one zero-traffic test, one racy readiness-fixture
correction, and the corresponding five test-only persistent-edge inventory
entries. There is no production runtime/collector change, unsafe occurrence,
trace or mutation change, root/admission change, finalization change, or
scheduler-semantic change.

The explicit I11D rerun policy therefore reuses the successful complete
aggressive workspace rather than spending roughly four hours repeating
unchanged direct-assembly computation. Current ordinary workspace checks, the
I11D.2e all-feature collector boundary, exact inventories, deterministic
protocol matrix, and feature-enabled production aggressive-entry smoke test
all pass.

This is evidence reuse under the recorded semantic trigger, not substitution
of repeated focused tests for a failed or stale full gate.

## Schedule and Lifecycle Closure

The certified production schedules are:

- serial collection before, between, and after compilation, reflection-store
  publication, diagnostic ingress, output delivery, settlement, and retained
  net observation;
- a synchronous collector observably waiting behind a worker's bounded managed
  access, then completing after that worker releases normally;
- collection proceeding beside host work which holds no mutator while passive
  managed finalization publishes no runtime, event, diagnostic, task, or
  replacement-allocation work;
- an external nonblocking request arriving after finalizer obligations are
  durable, coalescing into that collection without recursive collection or a
  lost later request;
- recoverable mark/finalizer panics preserving retry state and retiring their
  collector mutator, while irreversible topology publication poisons the heap
  and wakes waiters rather than reopening uncertain memory; and
- runtime/domain retirement making escaped public values inert, with terminal
  teardown waiting for active owner regions and supplying no finalizer mutator.

Every disputed order above is established by a probe, barrier, channel, or
authoritative state observation. Test repetition is not part of the proof.

## Ownership and Static Closure

The final exact primary ledgers remain:

| Ledger | Accepted surface |
| --- | --- |
| raw `core::Value` APIs | 518 total; 485 regional access, 28 collector primitives, five regional representations; zero violation/pending |
| persistent managed edges | 878 total; 148 production typed, 36 production erased, 680 test typed, 14 test erased; zero defect/pending |
| runtime-root publication | 272 exact occurrences; zero defect or nested active-access construction |
| mutator introduction | 534 exact occurrences; only two private higher-ranked direct collector admissions; zero production nested/pending entry |

Durable-owner, containment/capture, active external-owner, machine-state, WHNF
checkpoint, recursive-identity, runtime-cache, compiler-boundary,
resolved-call, and Gate G2 composition inventories also close. The checked
unsafe manifest matches current source. Managed destruction remains passive;
arbitrary active Rust ownership remains explicit and external rather than
hidden behind a managed root.

## Dynamic Unsafe-Boundary Evidence

I11D.2's exact tool matrix remains current because the later delta changes no
unsafe or production path:

- collector Miri under strict provenance passed 201 tests with three ignores,
  plus its isolated intentional process-lifetime leak fixture;
- eight selected production targets passed under Miri;
- the broad serial-boundary production fixture is an explicit Miri performance
  exclusion after fifteen CPU-minutes without a diagnostic, while its component
  paths pass under Miri and its complete behavior passes natively, ASan/LSan,
  and TSan;
- collector and production ASan/LSan matrices passed with leak detection
  retained except for the collector's isolated intentional-leak fixture; and
- collector and production TSan matrices passed without a race report.

Sanitizer success does not establish ordering; the deterministic schedule
fixtures above remain authoritative. The one Miri performance exclusion is a
documented tool-cost boundary, not a correctness exception.

## Reflection and Reproducibility

Collection is not a Glam semantic mutation. It neither advances runtime
observation epochs nor changes pure values, transaction contents, diagnostics,
or net topology. The serial production fixture verifies those boundaries
through the same runtime that owns reflection tasks and the shared reflection
store.

Reflection task scheduling and the arrival order of independent external
diagnostics remain intentionally outside pure assembly reproducibility. Gate G3
does not turn that scheduling into a deterministic contract. It certifies that
every scheduled task/result/diagnostic retained across collection has an exact
owner and that collector timing cannot observably influence pure evaluation.
Where reflection semantics require order—transaction commit order, explicit
choice order, source order, or a test's disputed lifecycle—the existing
protocol and deterministic fixtures continue to establish it.

## Final Verification Record

Fresh certification checks at `2e19bb29` passed:

```text
cargo fmt --check
    passed

cargo clippy --all-targets --all-features -- -D warnings
    passed

cargo test -q
    root library: 1,882 passed, 2 ignored
    every auxiliary target passed

cargo test --workspace -q
    root library and every auxiliary target passed
    glam-gc library: 204 passed, 2 scale-only ignored
    seven Loom models and all remaining collector/doc targets passed

scripts/check-interaction-net-profiling.sh
    21 named profiling regressions passed

cargo test -q --features aggressive-gc-verification --lib \
  repository_aggressive_mode_enables_each_production_runtime
    1 passed
```

I11D.3 immediately before certification also reran the checked unsafe manifest,
all 121 library plus eight integration inventory tests, all nine production
collection fixtures, and nine focused collector wait/finalizer/panic/retirement
fixtures.

## Gate Decision and Deferred Work

Gate G3 passes and Phase I11 is complete. The initial collector foundation now
meets its purpose: it can reclaim production lazy, promise, fixpoint, container,
and net cycles through an explicitly selected complete-runtime maintenance
boundary without weakening Glam semantics or external-owner lifecycles.

The following remain deliberately outside this gate:

- I12's authoritative runtime-maintenance activity/readiness integration and
  later policy decision for newly constructed runtimes;
- automatic threshold collection;
- concurrent or moving collection, generational policy, and weak/ephemeron
  representations;
- lifetime-branded managed pointer views; and
- broader value-representation and diagnostic-projection refinements.

No existing runtime changes policy as a consequence of certification.
Production remains `CollectionPolicy::NoAuto` until a later explicit plan
selects and verifies a different policy for newly constructed runtimes.
