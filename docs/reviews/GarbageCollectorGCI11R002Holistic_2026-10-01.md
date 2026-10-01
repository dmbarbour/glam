# Garbage Collector GCI11R-002 Holistic Review — 2026-10-01

Implementation baseline: `d2d27211`, after GCI11R-002A-H, I11D.1, nested
persistent-edge P3-P4/P5C, and D.2h.5 closure. Focused D.2h accounting is in
[`GarbageCollectorAggressiveD2h_2026-10-01.md`](GarbageCollectorAggressiveD2h_2026-10-01.md).

Status: complete. GCI2HR-001 through GCI2HR-008 reconcile GCI11R-002 with the
collector roadmap, integration plan, ownership ledger, persistent-edge and
scoped-pointer plans, value-representation plan, architecture guide, and
source map. No new implementation defect was found. Parent-plan drift is
repaired, and the forward path is now I11D.2a-I11D.2e, I11D.3a-I11D.3d, then
I11D.4 Gate G3 certification.

## Scope and Review Boundary

This review covers the complete GCI11R-002 remediation rather than only its
last trait/fixture cutover. It accounts for:

- A's failure matrix and attribution support;
- B-C's evaluator and closed-compiler ownership repairs;
- D's repository-wide raw-value and durable-owner transition, including the
  completed resumable-WHNF, callable-checkpoint, pure-net-construction, and
  persistent-edge subplans;
- E's fixture publication migration;
- F's schedule-fixture adaptation;
- G's subsystem closure; and
- H's ordinary/aggressive repository certification.

The nested WHNF and net-construction transitions already have mandatory phase
and holistic reviews. This review consumes those findings and their current
source inventories rather than pretending to re-review several weeks of
evaluator protocol work under a GC label. Its responsibility is the parent
claim: repository-wide aggressive collection now has a coherent ownership
boundary, and the remaining collector integration phases describe the work
which is actually left.

Current source and the dated D.2h closure remain authoritative. This review
also reran the complete inventory filter and the focused negative persistent-
edge trait contract. It did not rerun the roughly four-hour complete
aggressive workspace because no Rust implementation changed after its recorded
D.2h.4 pass.

## Holistic Plan-to-Implementation Accounting

| Area | Current disposition |
| --- | --- |
| GCI11R-002A-C | Complete. Attribution remained debug-only; evaluator poll state and closed compiler results now retain exact owners across their orchestration boundaries. |
| GCI11R-002D.1-D.2g | Complete. Every production raw-value boundary was either made regional under matching access or replaced by an exact durable owner. |
| GCI11R-002D.2c / resumable WHNF | Complete and independently reviewed. Canonical resumable state is retained by the semantic lazy/net owner or one aggregate durable demand root rather than replayed or scattered through fine-grained roots. |
| GCI11R-002D.2h / persistent edges | Complete. Ambient semantic duplication, equality, formatting, and pointer identity are gone; exact ledgers report zero defect or pending disposition. |
| GCI11R-002E-F | Complete. Test fixtures publish roots in the construction region, and concurrency claims use observed barriers/probes rather than repetition. |
| GCI11R-002G-H / I11D.1 | Complete. Ordinary and aggressive subsystem clusters and both complete workspaces pass under immutable production `NoAuto`. |
| Persistent-edge P5A/P5B | Partially open by design. Ordinary/aggressive behavior and parent reconciliation pass; focused Miri and release-cost/code-generation evidence are now explicit I11D.2 work. |
| Gate G3 | Open. I11D.2 dynamic tools, I11D.3 final static delta audit, and I11D.4 certification remain. I12 production maintenance is not authorized. |

## Current Architectural Result

One `EvaluationRuntime` remains one value domain with one non-moving STW
collector heap. Production policy remains immutable `NoAuto`; aggressive
collection is a private compile-time verification mode which collects before
eligible outer entries without becoming an embedding API.

Raw semantic values are regional. Durable boundaries use the public/runtime
root facade or a traced managed owner. Lazy, promise, metadata, function,
collection, WHNF-checkpoint, and core-net recursion reach exact managed edges.
Reflection checkpoints remain coordinator roots rather than being inserted
into value graphs. Host calls root declared semantic captures before ending
access and invoke opaque callbacks only afterward.

Persistent `Gc<T>` edges are pointer-sized and move-only. Duplication and
allocation identity require matching mutator access; registered roots and
public rooted handles retain their own deliberate clone semantics. This is a
stronger and more searchable bootstrap boundary, but not lifetime branding,
moving-GC readiness, or a claim that source scanners prove arbitrary Rust
lifetimes.

The final accepted ledgers report:

| Surface | Accepted result |
| --- | --- |
| Raw core-value API | 518 occurrences: 485 regional functions, 28 collector primitives, five regional aliases; zero violation/pending entry. |
| Persistent managed edges | 873 occurrences: 148 production typed, 36 production erased, 675 test typed, 14 test erased; zero defect/pending entry. |
| Runtime-root publication | 272 occurrences through two canonical constructors; zero defect. |
| Mutator introduction | 534 occurrences; direct admission remains confined to two higher-ranked gateways. |

These exact inventories are change detectors. Their semantic evidence comes
from focused liveness/reclamation tests, deterministic schedule fixtures, and
the ordinary/aggressive repository matrix.

## Findings

### GCI2HR-001 — GCI11R-002 expanded far beyond a feature toggle, justifiably

**Severity:** accounting finding.

**Disposition:** accepted.

The original finding was that aggressive collection existed only as a local
hook rather than a repository-wide mode. Enabling it repository-wide exposed a
deeper fact: numerous evaluator, compiler, fixture, and callback paths relied
on “root it later” behavior which the regional model never promised. Repairing
only the feature wiring would have produced a deterministic defect detector,
not a passing ownership boundary.

The resulting resumable evaluator and net-construction work was substantial,
but each transition eliminated replay, raw poll-spanning state, or root-in-
value-graph hazards which directly blocked the aggressive ownership contract.
Those subplans were reviewed independently. The parent plan correctly treats
their implementation as completed prerequisites rather than hiding them as
mere test repairs.

### GCI2HR-002 — No unaccounted unsafe or collection-policy drift was found

**Severity:** implementation audit result.

**Disposition:** accepted.

The D.2h interval adds no Rust `unsafe` site. Managed graph mutation remains
behind collector-owned edge transitions and reviewed net gateways. The
aggressive feature does not change a heap's immutable policy, expose a public
collection route, infer runtime quiescence, or add callback work to tracing or
finalization.

This review therefore found no reason to reopen GCI11R-002 or I11D.1. It also
does not infer safety solely from a green aggressive suite: the remaining
unsafe-boundary and static audits stay explicit Gate G3 prerequisites.

### GCI2HR-003 — Parent status and architecture prose had fallen behind

**Severity:** medium documentation drift.

**Disposition:** resolved in this review.

The roadmap, integration status table, ownership ledger, source map, and
evaluation architecture still described some combination of D.2d-D.2g,
persistent-edge P4/P5, repository-aggressive closure, or ordinary `Gc<T>`
copy/equality as pending. The plan index also listed completed WHNF and net
subplans as active.

Those documents now distinguish:

- completed I11D.0-I11D.1 regional and repository verification;
- open I11D.2-I11D.4 Gate G3 work;
- the completed move-only persistent-edge representation from its remaining
  Miri/cost evidence; and
- the deferred optional `ScopedGc` working-view experiment from the already
  completed stored-edge cutover.

### GCI2HR-004 — I11D.2 was too broad to execute safely as one checkpoint

**Severity:** planning risk.

**Disposition:** resolved by partitioning.

I11D.2 now begins with a tool/target matrix, then separates focused Miri,
AddressSanitizer, ThreadSanitizer, and persistent-edge cost/code-generation
closure. Unsupported tool/target combinations must be recorded explicitly;
ordinary repetition is never substituted for Miri or a sanitizer.

The Miri matrix covers both isolated collector operations and production-
runtime ownership paths. Sanitizers remain defect detectors, not proofs of
concurrency ordering. Existing barriers, condition-variable probes, and model
fixtures remain authoritative for those contracts.

### GCI2HR-005 — I11D.3 must be a delta audit, not a second D.2h migration

**Severity:** planning precision.

**Disposition:** resolved by partitioning.

The final static audit now has four steps: source delta, authoritative ledger
reconciliation, protocol-boundary audit, and focused closure. It consumes the
D.2h exact surfaces as its baseline, reviews any repair introduced by dynamic
tools, and refuses to accept a changed count or fingerprint mechanically.

This preserves the purpose of final certification without repeating the
thousands of already reviewed fixture migrations or implying that a source
scanner alone proves the runtime behavior.

### GCI2HR-006 — Certification now has an explicit aggressive-rerun trigger

**Severity:** medium verification-cost issue.

**Disposition:** resolved in I11D.4.

D.2h's full aggressive workspace is authoritative evidence for its exact
implementation baseline, but costs roughly four hours due to direct-assembly
subprocesses. I11D.4 may cite that pass across documentation changes,
tool-orchestration additions, and explicit unsupported-tool records. It must
rerun the complete aggressive workspace after any runtime/collector repair or
change to unsafe tracing/mutation, root/admission, finalization, or scheduler
semantics. Relevant focused aggressive fixtures and routine checks continue to
run after every implementation edit.

This is evidence reuse, not a relaxation: the trigger follows the semantic
surface which the full gate certifies.

### GCI2HR-007 — Deferred pointer/value plans now start from the real boundary

**Severity:** low future-plan drift.

**Disposition:** resolved.

The scoped-pointer plan no longer speaks as if move-only stored edges are
future work. That transition is complete. `ScopedGc<'mutator, T>` remains a
possible compile-time safety enhancement for temporary working views after
Gate G3, normally coordinated with Value Representation Refinement to avoid
migrating the large compatibility representation twice.

Value Representation Refinement likewise now treats move-only persistent
edges as current fact. Neither future plan is pulled into Gate G3, and neither
current review claims support for relocation or concurrent marking.

### GCI2HR-008 — Gate G3 remains meaningfully open

**Severity:** gate accounting.

**Disposition:** accepted.

GCI11R-002 completion proves the repository-wide regional ownership contract
under the current non-moving STW collector and its hostile admission mode. It
does not provide Miri/sanitizer results, the final unsafe/trace/lock delta
audit, or a dated certification which accounts for all I11 schedules.

Accordingly, I11 is still pending, production remains `NoAuto`, and I12's
runtime operational-activity/maintenance design remains unauthorized. This is
the correct stopping point; proceeding to automatic or concurrent collection
would conflate stronger future protocols with the boundary just certified.

## Revised Forward Path

1. **I11D.2a:** inventory installed dynamic tools and select exact supported
   collector/runtime targets.
2. **I11D.2b-I11D.2d:** run focused Miri, ASan, and TSan work with explicit
   unsupported dispositions and repair only demonstrated defects.
3. **I11D.2e:** close persistent-edge P5B layout/traffic/code-generation cost.
4. **I11D.3a-I11D.3d:** audit the source delta, rerun and review the exact
   ledgers, reconcile protocol boundaries, and close focused I11 tests.
5. **I11D.4:** apply the aggressive-rerun trigger, run the required routine and
   workspace checks, and publish the dated Gate G3 certification.

Only after Gate G3 should the project begin I12's explicit runtime maintenance
and policy reviews. Scoped-pointer branding, compact value representation,
concurrent collection, and pure-effect access fusion remain separate future
transitions.

## Verification Record

Fresh review checks:

```text
cargo test -q inventory
    121 library tests passed
    8 selected integration tests passed

cargo test -q persistent_edge_standard_trait_cutover_is_closed
    1 focused collector test passed
```

Documentation changes pass `git diff --check`. The complete routine,
profiling, ordinary-workspace, and aggressive-workspace evidence remains the
unchanged D.2h.4/D.2h.5 baseline. No implementation change was made during
these two reviews.
