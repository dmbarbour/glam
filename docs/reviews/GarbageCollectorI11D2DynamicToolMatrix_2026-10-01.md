# Garbage Collector I11D.2 Dynamic Tool Matrix — 2026-10-01

Baseline: `6e3ffddb`, after GCI11R-002 and its parent plans were reconciled.

Status: I11D.2a complete. The host and target matrix is selected, but the
current rebuilt container has no nightly toolchain. Miri, AddressSanitizer,
and ThreadSanitizer are therefore **unavailable in this environment**, not
demonstrated unsupported. I11D.2b-I11D.2d require installation of one exact
nightly toolchain with Miri and `rust-src` before their result records can be
produced.

## Environment Snapshot

```text
host: x86_64-unknown-linux-gnu
rustc: 1.98.1 (48a229cea 2026-09-01), LLVM 22.1.8
cargo: 1.98.1 (797e8a9bc 2026-08-05)
active toolchain: stable-x86_64-unknown-linux-gnu
installed stable components: cargo, clippy, rust-analyzer, rust-src,
                             rust-std, rustc, rustfmt
nightly toolchain: not installed
nightly Miri component: not installed
nightly rust-src component: not installed
```

`cargo miri --version` fails because Miri is unavailable for the active stable
toolchain. Both existing sanitizer scripts require `cargo +nightly`,
`-Zsanitizer`, `-Zbuild-std`, and nightly `rust-src`, so neither sanitizer can
be invoked from the current installation.

This is environment drift from the 2026-09-11 post-I11 review, which recorded
an installed nightly, invocable Miri, and exposed sanitizer flags. That older
observation and the Gate G1 dynamic-tool passes remain historical evidence for
the collector scripts, but they are not current Gate G3 results.

Before I11D.2b, install one nightly toolchain with `miri` and `rust-src` and
record its full `rustc -Vv` output. A missing toolchain must not be written up
as an unsupported Rust target, and repeated stable tests must not substitute
for a dynamic-tool result.

## Selected Target Matrix

| Tool | Collector target | Production-runtime target | Current disposition |
| --- | --- | --- | --- |
| Miri with strict provenance | `crates/glam-gc/scripts/check-miri.sh`, which runs the collector library and isolates its one intentional process-lifetime leak fixture | Named serial ownership/root/mutation/collection tests, followed separately by the worker/finalizer deterministic probes and repository-aggressive smoke test listed below | Unavailable: nightly/Miri absent. Target support not yet revalidated on the replacement toolchain. |
| AddressSanitizer plus LeakSanitizer | `crates/glam-gc/scripts/check-sanitizer.sh address`, including the documented isolated intentional-leak exception | Root projection, mutation gateway, and the complete `managed_collection_tests` module under ASan; no production leak suppression is selected | Unavailable: nightly/rust-src absent. The host/commands previously passed Gate G1, but require a fresh result. |
| ThreadSanitizer | `crates/glam-gc/scripts/check-sanitizer.sh thread` | Root/mutation paths plus the complete production managed-collection schedule module under TSan | Unavailable: nightly/rust-src absent. Ordering contracts still rely on their existing probes and barriers. |
| Loom | Existing collector Loom models in `crates/glam-gc/scripts/check.sh` | No production-runtime target selected; I11 added no model-sized synchronization primitive requiring a new Loom abstraction | Available on stable, but outside I11D.2's dynamic unsafe-boundary obligation. |

The sanitizer scripts deliberately exclude the separate Loom scaffold because
Loom's own process-lifetime allocation and ASan stack-switch warning are known
tool incompatibilities. Stable Loom remains an independent synchronization
model rather than being smuggled into a sanitizer result.

## Miri Production-Runtime Matrix

Run the serial and single-thread ownership paths first, one exact target at a
time, so a tool limitation or performance problem has an attributable result:

```text
core::managed::value_node::tests::prepared_root_projection_nests_inside_one_runtime_access_region
core::managed::tests::runtime_value_access_routes_borrowed_edge_sets_to_the_collector_gateway
api::tests::managed_collection_tests::production_collection_preserves_each_serial_boundary
api::tests::managed_collection_tests::production_runtime_reclaims_each_recursive_identity_family
api::tests::managed_collection_tests::runtime_retirement_before_collection_leaves_public_value_inert
api::tests::managed_collection_tests::passive_finalization_produces_no_runtime_work
```

Then run the synchronization/probe paths independently:

```text
api::tests::managed_collection_tests::collection_interleaves_with_worker_quantum_without_lost_work
api::tests::managed_collection_tests::external_request_during_finalization_is_coalesced
```

Finally run the repository feature smoke target with
`--features aggressive-gc-verification`:

```text
api::tests::managed_collection_tests::repository_aggressive_mode_enables_each_production_runtime
```

The first group establishes production root publication/projection, exact
mutation observation, serial graph preservation/reclamation, retirement, and
passive finalization. The second group executes the deterministic collector-
wait and Finalizing-phase probes. Miri does not replace their ordering proof;
it checks the unsafe memory behavior of the already latched schedules. If a
probe is unreasonably slow or rejected by Miri, record that exact result and
retain the native latched proof rather than silently dropping the target.

## Sanitizer Production-Runtime Matrix

For each supported sanitizer, build the root library with the same host target
and `-Zbuild-std` protocol used by the collector scripts, then run these three
filters:

```text
core::managed::value_node::tests::prepared_root
core::managed::tests::runtime_value_access
api::tests::managed_collection_tests::
```

The final filter currently contains nine ordinary tests. It covers serial
production graph preservation, recursive-family and compatibility-container
reclamation, external-owner retirement, worker/collector exclusion, runtime
retirement, passive finalization, request coalescing during finalization, and
the explicit aggressive-entry hook. The feature-only repository-aggressive
smoke target is added in a separate feature-enabled invocation.

ASan keeps leak detection enabled for all production-runtime targets. The
collector script's one leak exception applies only to
`forgotten_scoped_allocator_does_not_retain_its_heap`; it must not become a
workspace-wide option. TSan results are race detectors and do not replace the
tests' authoritative schedule observations.

## Stable Target Validation

The selected filters exist and pass on the current stable toolchain:

```text
cargo test -q --lib managed_collection_tests::
    9 passed
cargo test -q -p glam-gc --lib root::tests::
    11 passed
cargo test -q -p glam-gc --lib pointer::tests::
    8 passed
cargo test -q -p glam-gc --lib mutation::tests::
    5 passed
cargo test -q --lib prepared_root_projection_nests_inside_one_runtime_access_region
    1 passed
cargo test -q --lib runtime_value_access_routes_borrowed_edge_sets_to_the_collector_gateway
    1 passed
cargo test -q --features aggressive-gc-verification --lib \
  repository_aggressive_mode_enables_each_production_runtime
    1 passed
```

These runs validate target selection only. They are not Miri or sanitizer
evidence.

## Exit and Next Step

I11D.2a is complete because the environment, missing prerequisites, exact
collector entry points, production-runtime target families, and unsupported-
versus-unavailable distinction are recorded. I11D.2b begins by provisioning
and fingerprinting a nightly toolchain, then runs the existing collector Miri
script and the serial production-runtime matrix before attempting the two
threaded probe targets.
