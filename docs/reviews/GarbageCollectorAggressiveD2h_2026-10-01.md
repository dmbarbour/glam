# Garbage Collector Aggressive D.2h Review — 2026-10-01

Implementation baseline: `d2d27211`, the completed
GCI11R-002D.2h.5 closure record. The implementation interval begins after
GCI11R-002D.2g at `edf1ecf7` and contains 79 commits touching 127 files, with
9,718 insertions and 6,576 deletions.

Status: complete. D2HR-001 through D2HR-007 account for the expansion of
D.2h, accept its intentional representation and API changes, identify its
remaining proof limits, and assign all forward work to named parent phases.
No D.2h remediation remains open.

## Scope and Method

This review is deliberately narrower than the GCI11R-002 and collector-plan
review which follows it. It asks whether
[`GCI11R-002D.2h`](../plans/GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md)
implemented the zero-violation and ownership closure it advertised, whether
its unexpectedly large implementation remained inside that purpose, and
whether its local plan still points at the correct next work.

The review compared the D.2h plan and dated closure record with:

- the complete `edf1ecf7..d2d27211` source and commit delta;
- the current raw-value, persistent-edge, root-publication,
  mutator-introduction, durable-owner, containment, callback, machine-state,
  recursive-identity, and resolved-call inventories;
- the standard-trait negative contract on `Gc<T>` and its managed facades;
- the ordinary and aggressive verification record; and
- the remaining persistent-edge, scoped-pointer, and Gate G3 plans.

The current review reran `cargo test -q inventory` and the collector's
`persistent_edge_standard_trait_cutover_is_closed` test. All 121 library
inventory tests, the eight integration tests selected by that filter, and the
focused collector trait test pass.

## Plan-to-Implementation Accounting

| Checkpoint | Current disposition |
| --- | --- |
| D.2h.0 | Complete. The list-effect trace omission and compiler exact-target wait/admission witnesses were repaired and latched. The plan records the completed work in the numbered closure record, although the introductory bullet lacks the same inline completion label. |
| D.2h.1 | Complete. The pre-cutover ledgers established 32 raw API violations and 77 persistent-edge defects, with no reclassification escape. |
| D.2h.2 | Complete. Ambient semantic `Clone`, `Debug`, equality, and unmanaged pointer identity were removed from raw values, runtime sources, managed facades, and `Gc<T>`. Callers now use explicit regional duplication, observation, or identity. |
| D.2h.3 | Complete. The raw API, persistent-edge, root-publication, and mutator-introduction ledgers close at zero defect/pending status. Canonical edge-free atoms no longer consume six permanent roots. |
| D.2h.4 | Complete. Exact source closure, production-shaped dynamic closure, routine workspace closure, the complete aggressive workspace, and final source reconciliation all passed. |
| D.2h.5 | Complete. The dated closure review records the final accepted surfaces and correctly leaves Miri/sanitizers, the release-cost audit, the final static audit, and Gate G3 certification open. |

The 79-commit expansion is mostly the implementation consequence of D.2h.2.
Removing traits from recursive `Value`, failures, operators, managed facades,
and `Gc<T>` caused the Rust compiler to expose every implicit duplication,
comparison, and diagnostic consumer in production and fixtures. Closing that
surface without temporary compatibility traits required subsystem-by-
subsystem migration and independently compiling test slices. That is large,
but it is the expected closure shape of the selected design rather than an
unrelated feature expansion.

## Accepted Current Boundary

The settled D.2h rule is regional rather than value-local: a raw
`core::Value`, `Gc<T>`, or managed semantic edge may be duplicated, compared,
projected, or recursively formatted only while matching runtime value access
is held. A durable boundary retains an exact managed root or places the edge
beneath an exhaustively traced managed owner. The public API continues to
expose opaque rooted values rather than raw semantic identities.

The compiler enforces an important negative half of this contract. `Gc<T>`
and the semantic facades no longer implement ambient `Copy`, `Clone`,
`PartialEq`, `Eq`, or `Debug`, and `Gc<T>` no longer offers unqualified pointer
identity. Explicit `duplicate_in` and `same_allocation_in` operations bind
those actions to a matching mutator. Source inventories enforce the positive
half by classifying every raw API, persistent edge, root publication, and
mutator introduction.

This is not yet a lifetime-branded pointer proof. An unbranded `Gc<T>` can be
carried beyond the access region which produced it, although it cannot be
safely dereferenced or duplicated there through the public collector API. The
deferred scoped-pointer plan may make this harder by construction; D.2h's
claim is the current regional contract plus exact source and aggressive
dynamic closure, not a proof suitable for concurrent or moving collection.

## Findings

### D2HR-001 — The phase expansion is justified compile closure

**Severity:** accounting finding.

**Disposition:** accepted.

D.2h grew from a short “remove traits and close the ledgers” checkpoint into
79 commits because the removed traits were present on the recursive semantic
currency of the repository. Their removal deliberately broke production
callers, generic interaction-net fixtures, evaluator/reflection machines,
compiler fixtures, diagnostics, and assertion helpers until each supplied the
correct access authority or durable owner.

The diff adds no Rust `unsafe` site and no new Glam language behavior. Its
semantic effects are ownership enforcement, explicit observation, and the
correction of defects exposed by those constraints. The small checkpoint
description substantially underestimated mechanical reach, but the resulting
work remained within the ownership-closure purpose.

Future cross-cutting trait removals should be partitioned from the start by
production seam, fixture seam, negative compiler contract, exact inventory,
and dynamic certification. D.2h eventually adopted that structure, but only
after the phase had already expanded.

### D2HR-002 — Canonical atom root removal is intentional representation drift

**Severity:** low, beneficial drift.

**Disposition:** accepted.

D.2h.3 removed six permanent roots for edge-free canonical atoms and now
constructs those atoms inside caller-held access. This was not explicit in the
original closure bullet, but it is the simpler implementation of the intended
boundary: an edge-free immediate does not need a durable managed owner merely
to avoid opening a regional constructor.

The transition briefly exposed an atom-identity regression when an already
interned atom key was re-interned. Independent tuple-lowering and diagnostic-
ingress witnesses caught it, and the shared constructor now preserves atom
identity. The final focused and full gates include that correction.

### D2HR-003 — Rust formatting was narrowed without weakening Glam diagnostics

**Severity:** medium API decision.

**Disposition:** accepted.

`EvaluationFailure::Display` no longer traverses a structured semantic error.
Plain host text remains available, while a structured evaluation failure uses
an edge-free compatibility classification. Full messages, contexts, and
viewer policy require explicit runtime access and remain part of Glam's
diagnostic path.

This is the correct division: Rust `Display` cannot carry a mutator and should
not force or flatten a potentially lazy Glam diagnostic merely for incidental
formatting. The broader public diagnostic compatibility cleanup remains a
separate deferred item and is not needed to reopen D.2h.

### D2HR-004 — The ownership proof remains audit-backed, not lifetime-branded

**Severity:** accepted bootstrap limitation.

**Disposition:** closed by explicit deferral.

The exact inventories and aggressive mode detect the currently known ways a
raw managed edge can cross a region without an owner. They are strong change
detectors, but the source scanners necessarily recognize Rust syntax and
reviewed conventions rather than proving a higher-ranked lifetime for every
nested edge.

The negative trait cutover materially reduces the accidental surface, and the
two higher-ranked gateways confine direct mutator admission. A later
[`GarbageCollectorScopedPointerSafety_2026-09-09.md`](../plans/GarbageCollectorScopedPointerSafety_2026-09-09.md)
transition may add lifetime-branded borrowed pointers. It should be
coordinated with value-representation work rather than inserted into Gate G3:
the current non-moving STW collector does not require it for correctness.

### D2HR-005 — Exact source inventories are justified but deliberately brittle

**Severity:** maintenance cost.

**Disposition:** accepted through Gate G3.

The syntax-backed inventories carry exact counts and fingerprints. This
creates routine maintenance when legitimate APIs move, but also prevented the
phase from “fixing” failures by silently reclassifying a new raw accessor,
root constructor, mutator gateway, or persistent edge. Compiler-negative trait
tests and aggressive collection provide independent evidence, so correctness
does not rest on one scanner.

I13 may consolidate these ledgers after Gate G3 and stable maintenance-policy
work. Consolidation must preserve the zero-unclassified rule and independent
dynamic witnesses; it should not weaken the latches merely to reduce fixture
volume during the active transition.

### D2HR-006 — The full aggressive gate needs a change-trigger policy

**Severity:** medium verification-cost issue.

**Disposition:** assigned to I11D.4.

The final complete aggressive workspace gate passed, but five direct-assembly
subprocesses made it roughly a four-hour check. This is valuable certification
evidence, not an appropriate unconditional loop after documentation-only or
dynamic-tool-only work.

I11D.4 should reuse the dated D.2h result when intervening changes are limited
to documentation, test orchestration, or unsupported-tool recording. It must
rerun the complete aggressive workspace if I11D.2 or I11D.3 changes runtime or
collector code, unsafe/tracing/mutation/root/admission/finalization behavior,
or scheduler semantics. Routine checks and focused aggressive witnesses still
run for every relevant implementation edit.

### D2HR-007 — Local plan state is complete but parent summaries are stale

**Severity:** documentation drift.

**Disposition:** assigned to the holistic GCI11R-002 review.

The D.2h detailed record and dated closure review agree with current source.
Several parent summaries still describe D.2d-D.2g, P4/P5, or I11D.1 as open,
and some future-plan prose still describes managed edges as cheap ambient
copies. Those statements predate D.2h and must be reconciled before I11D.2 so
that the next phase does not repeat completed migration or revive removed
semantics.

## Verification and Closure

Fresh focused review evidence:

```text
cargo test -q inventory
    121 library tests passed
    8 selected integration tests passed

cargo test -q persistent_edge_standard_trait_cutover_is_closed
    1 focused collector test passed
```

The authoritative full dynamic evidence remains the D.2h.4 record: formatting,
denied-warning all-target/all-feature Clippy, ordinary workspace tests,
interaction-net profiling, and the complete aggressive workspace passed. The
accepted final exact surfaces remain 518 raw APIs, 873 persistent edges, 272
runtime-root publications, and 534 mutator introductions at their recorded
fingerprints, all with zero defect or pending disposition.

D.2h is therefore closed. The next work is not another regional-value
migration; it is I11D.2 dynamic unsafe-boundary verification, followed by the
delta-oriented I11D.3 audit and I11D.4 Gate G3 certification.
