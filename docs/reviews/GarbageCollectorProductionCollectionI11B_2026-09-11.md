# Glam GC Controlled Production Collection Review — 2026-09-11

Baseline: `585cfec`, the completed I11A / Gate G2 certification. The private
maintenance seam and production fixtures reviewed here are I11B.1-I11B.3.

Status: complete. Controlled serial full collection passes over the actual
production runtime graph. Production heap construction remains
`CollectionPolicy::NoAuto`; no public collection API or automatic collection
path was introduced. I11C still owns worker and finalizer concurrency.

## Scope and Method

I11B introduced one crate-private `EvaluationRuntime` maintenance operation
and exercised it only where the test owns a stable serial boundary. The
operation delegates to the runtime value domain's subordinate collector hook;
it does not infer readiness, take settlement authority, or alter observation
state. This separation is deliberate until I12 integrates collection activity
with ordinary runtime maintenance.

The serial-boundary fixture uses a complete zero-worker production runtime,
not an isolated core factory. It builds a `.g` module and managed core net,
attaches a same-runtime assembler as a logger-facing service session, routes a
structured diagnostic through the production ingress, commits reflection and
output-event roots, delivers the output, and settles a readiness snapshot.
Full collection runs between those operations rather than inside callbacks or
mutator regions.

## Serial Boundary Results

`production_collection_preserves_each_serial_boundary` establishes:

- a compiled assembly result remains evaluable after collection;
- committed reflection state survives both collection and settlement;
- queued output is retained and delivered exactly once;
- the logger-facing diagnostic remains structured and decodable by the
  same-runtime service after collection;
- collection changes neither readiness work generation nor observation epoch,
  and a readiness snapshot captured before collection still validates;
- the retained production net's topology revision does not change; and
- a settled report continues to retain its reflection roots across a later
  collection.

The service fixture intentionally exercises the library's production
diagnostic ingress and same-runtime evaluation-session ownership rather than
the executable's rendering policy. Rendering callbacks are external policy,
not managed semantic edges, and do not need collection authority for this
serial graph proof.

## I5-I10 Ownership Reconciliation

| Prior phase | Production I11B evidence |
| --- | --- |
| I5 recursive identities | Public promise self-cycle, access-lazy/promise cycle, and core-net/promise cycle exercise every mutable managed identity. Each retains one final root, marks its exact closed graph, and reclaims the exact graph after root removal. |
| I6 compatibility edges | List, dictionary, and application cycles reach their promise identity through the wildcard-free compatibility visitor. |
| I7 persistent collections | Independent public list and persistent dictionary fixtures prevent one container adapter from concealing the other. |
| I8 interaction nets | The public `Assembler::net` fixture closes and reclaims a net/promise cycle; the serial fixture separately proves collection leaves a live net revision unchanged. |
| I9 runtime roots | `public_values_use_inline_or_shared_registered_roots` now collects through `EvaluationRuntime`: inline values add no root, public clones share one root, and final root removal permits exact reclamation. Runtime transaction, event, diagnostic, module, service-session, and settlement roots are exercised by the serial fixture. |
| I10 external owners | A production opaque payload retained through an effect-token owner survives collection. Dropping the token, collecting its passive shell, explicitly draining its external owner, collecting the payload shell, and draining the opaque owner produces the documented two-stage retirement without pretending either active owner is a managed edge. |

No managed family, compatibility owner, or conservative external boundary from
the Gate G2 inventory lacks a production-runtime outcome. The tests use fresh
runtimes for independent cycle families so exact marked and finalized counts
cannot be satisfied accidentally by another fixture.

## Drift and Decisions

- The phase was partitioned into a private seam, serial preservation fixture,
  ownership matrix, and closure checkpoint because the original single phase
  combined API boundary, lifecycle semantics, and a broad verification matrix.
- The core factory retains a private subordinate hook because isolated
  collector fixtures still need it. The production test entry is the runtime
  operation, preventing new whole-runtime tests from selecting collection via
  a nested implementation detail.
- Collection is not recorded as a semantic runtime mutation. The unchanged
  readiness stamp and successful post-collection settlement validation make
  that policy executable.
- I11B is deliberately serial. It makes no claim about collection while a
  worker owns progress, a delivery callback is running, or finalization is
  concurrently disturbed; those are I11C obligations.

No unresolved accidental implementation drift was found.

## Verification

Verification passed on 2026-09-11:

- `cargo fmt --check`;
- `cargo clippy --all-targets --all-features -- -D warnings`;
- `cargo test -q -p glam-gc --lib` (193 passed, 2 ignored); and
- `cargo test -q` (1,454 main library tests plus every auxiliary target).

The phase adds no unsafe block or public API. Gate G3 remains closed pending
I11C, the post-I11 review, and I11D's focused Miri/sanitizer and final
certification work.
