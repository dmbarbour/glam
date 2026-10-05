# Documentation Disposition — 2026-10-05

Status: maintainer answered §7 on 2026-10-05 (see §8). Execution is in
progress, in the waves of §1.4. This report is the working plan for the
cleanup and retires when it finishes. Line numbers are as of `f3b22303` and
will drift.

Scope: the tree holds **23** files in `docs/plans/`, including `README.md` and
excluding this report, and **55** in `docs/reviews/`. That is 78 docs and
about 3.04 MB. The brief estimated 22 + 59.

## 1. Summary

### 1.1 Counts

| Disposition | Plans | Reviews | Total | Bytes |
| --- | --- | --- | --- | --- |
| KEEP | 10 | 2 | 12 | ~0.26 MB |
| EXTRACT-THEN-RETIRE | 10 | 27 | 37 | |
| MERGE | 1 | 1 | 2 | |
| RETIRE | 2 | 25 | 27 | |
| **Total** | 23 | 55 | 78 | ~3.04 MB (~2.78 MB retirable) |

### 1.2 Headline findings

- **Standing docs contain stale statements** (§5.1). Fix these whatever
  happens to the history docs. Several of them are the very destinations
  this cleanup would rely on:
  - the removed `CoreRuntimeNet` weak observer;
  - the removed reflection "stable task observation" sidecar;
  - "exclusive mutation admission" for collection;
  - glam-gc `SAFETY.md` still calling the `Gc<T>` trait cutover "pending";
  - "bounded standard-effect fusion".
- **The managed-value layer has no standing doc.** Its rationale is spread
  across the 451 KB integration plan, the ownership ledger, 8 I-series
  reviews and the gate reviews. A new `docs/architecture/values.md` blocks
  about 6 retirements. The holistic review's X8 already recommends it, and
  §3 gives a 14-section outline.
- **The collector's safety ledger defers to a history doc.**
  `crates/glam-gc/SAFETY.md:1491-1496` says the governing invariants "are
  maintained in" the Roadmap plan. That must be inverted before the Roadmap
  retires.
- **Measurements have no standing home.** This covers:
  - the G0 baseline;
  - the C8 numbers;
  - the W6G.4/W9 Callgrind and DHAT series and recipe;
  - the RuntimePolicy hello-world timing.

  Holistic X3 plans a performance harness (P1 item 5). Create
  `docs/Performance.md` with it, and keep the two recipe-holding reviews
  until then.
- **53 ADR entries are drafted** (§4): 46 accepted, 6 pending a maintainer
  decision, 1 proposed. Another 6 superseded decisions are listed in §4.9.
  - Only 9 of the accepted ones name the maintainer as decider.
  - The rest are agent decisions that plans and reviews recorded. §7 Q4 asks
    whether to ratify them.

### 1.3 Proposed target structure

| Concern | Proposal | Trade-off |
| --- | --- | --- |
| **ADR home** | **One log, `docs/Decisions.md`** (recommended), grouped by subsystem. Each entry gets a descriptive heading, used as its anchor; a date, decider and status line; and 3–8 lines. Standing docs keep the *rule*; the log keeps the *why* and the rejected alternatives. Split into `docs/decisions/<subsystem>.md` past about 60 entries or 40 KB. | One file is easy to scan and grep, needs no ceremony, and avoids recreating the doc-count sprawl. Per-file ADRs (`docs/decisions/NNNN-slug.md`) give per-decision `git log` and fewer merge conflicts between concurrent agents. But ~54 small files is exactly the tracking problem this cleanup is solving. |
| **Open-work index** | **Make `docs/plans/README.md` the single index**, with these sections: Active plans; Active reviews; Deferred plans; Open items without a plan (one line each, with a source link); Retired history (doc → last commit → label family). | No new file, and it is already the place agents look. "Open items without a plan" must stay short: an item moves into a plan as soon as one owns it. |
| **Reviews index** | **None.** A review retires once its findings are resolved or moved; the active ones are listed in the README above. | A separate reviews index would just be another file to keep current. |
| **New standing docs** | `docs/architecture/values.md` (§3 outline). In `crates/glam-gc/SAFETY.md`, restate "Governing Invariants" and add "Non-goals and Deferred Work". In `crates/glam-gc/VERIFY.md`, add "Gate history", the G0 numbers, the C8 numbers and a "Production-runtime dynamic matrix". `docs/Performance.md` comes with P1-5. | The values doc is the one large writing task. Everything else is a paragraph or a table. |
| **Labels** | **(b) rewrite comments to state their purpose, plus (c) git history**, through a family → doc → hash table in the README's Retired history (§6). No per-label glossary. | About 51 production comment lines plus 42 lint `reason` strings remain once the inventories retire. The holistic P1-7 item already rewrites "until milestone" reasons and renames milestone tests. |
| **Delete vs archive (X8, decision 5)** | **Delete** (recommended). Details follow. | |

**Delete vs archive.**

| | Delete (git keeps history) | Archive (`docs/archive/`, in tree) |
| --- | --- | --- |
| Search noise | Gone. X8 counted 6 current hits for `rg -w RuntimeValueAccess` against 123 in history. | Stays unless every tool honours a `.ignore`. Agents using `grep -r` still hit it. |
| Misleading text | Gone. Several retirable docs now say false things in the present tense: the per-entry aggressive mode, settlement comparing revisions, the sidecar. | Stays greppable and still misleads. |
| Recovery | `git show <hash>:docs/plans/<file>` using the Retired history table. | Open the file. |
| Links | Links from kept docs must be repointed (§2 lists them). | Relative links survive only if the plans/reviews layout is preserved. |
| Precedent | The maintainer deleted plans and reviews on 2026-08-19, 08-31 and 09-23 (`233fde61`, `d35d899e`, `4e1d7925`). `plans/README.md:9-10` already allows deletion. | None. |
| Cost | One-time hash table. | A permanent third tier that is neither authority nor gone, which is the ambiguity we are trying to remove. |

Recommendation: delete. Record in `plans/README.md` the retention rule X8
proposes:
1. the doc's durable decisions are in standing docs or `Decisions.md`;
2. no code, test or current doc references it;
3. its last commit hash is listed.

### 1.4 Suggested retirement waves

1. **Fix the stale standing docs** in §5.1. This is independent of the
   waves that follow.
2. **Retire the 27 RETIRE docs.** Each is either covered by standing docs or
   duplicated in a doc that stays until a later wave. First repoint
   `VERIFY.md:935` and `SAFETY.md:133` (the G1 review).
3. **Write `docs/Decisions.md`** from §4, and make the small standing-doc
   additions in §3. This retires about 25 EXTRACT docs and merges G0/C8 into
   `VERIFY.md`.
4. **Write `docs/architecture/values.md`.** This retires the integration
   plan, the ownership ledger, the I13 cleanup inventory and the
   readiness/08-25 reviews.
5. **Create `docs/Performance.md` with the P1-5 harness.** This retires the
   resumable-WHNF holistic review and the W6G.4 review.
6. **Retire active docs as they finish.** The parallel review goes first,
   then the polarity plan, the panic plan, and finally the holistic review.

## 2. Disposition Table

Legend:
- **E-T-R** = EXTRACT-THEN-RETIRE.
- **Refs**: references *outside* `docs/plans|reviews` that would dangle, plus
  label citations in code. History-to-history links are not listed, because
  they all retire together.
- **Commit**: the last commit touching the doc, for the Retired history
  table.

Paths are relative to `docs/plans/` (P) or `docs/reviews/` (R).

### 2.1 Index and active work

| Doc | Kind | Status | Disposition | What to extract → destination | Refs | Conf | Commit |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P `README.md` | index | current | **KEEP** | Restructure as the open-work index (§1.3); add the retention rule and the Retired history table. 4 plans are missing from it today: G0 baseline, ownership ledger, public value access inventory, resumable-WHNF holistic review plan. | — | high | cd605409 |
| P `NetPolarityChecker_2026-10-05.md` | plan | active. Slice 1 done; slice 2 in progress in the worktree during this survey (`src/interaction_net/polarity.rs`, test-build checks at `try_finish`). | **KEEP** | At close, move to `agent_context/interaction_nets.md` "Polarity": the checker algorithm, the evaluator-only signs, the `NetBuildError::Polarity` contract, and erasers being skipped in conflict diagnostics. Fix `:89-90` ("planned checker … will enforce"). Record the GAL deferral in `Design.md:439-442`. | `agent_context/interaction_nets.md:89` | high | 13082fe1 |
| P `UserInputPanicSafety_2026-10-04.md` | plan | active. Steps 1–5 done; interaction-net inspection remains and follows polarity. | **KEEP** | At close, extract the decisions in §4 ("Panics are bugs…", "Poison recovery…", "Discover panics…") and their uncovered halves (§3.1). Reconcile with polarity on disconnected subnets (§5.3). | `AgentContext.md:74` | high | d5cf8c6f |
| R `HolisticArchitecturePrePerformance_2026-10-03.md` | review | active backlog. P0 is done except the N8 checker. P1 items 5–8, P2 items 9–12 and items 13–16 are open. Maintainer decisions 2, 3, 5, 6 and 7 are open, and decision 4's surviving set is unchosen. | **KEEP** | Now: add status notes for X1, X2 (aggressive-GC fixed in `df60695d`), X4 and S3. At close: move open findings to owning plans (X3 harness, X5/V4 scaffolding, VRR V-1, net perf, CG prerequisites); decisions go to §4. | none | high | f3b22303 |
| R `ArchitectureAndVerification_2026-10-03.md` | review | active; first to retire. AR-001 and AR-002 resolved 2026-10-05. AR-003 and AR-004 are done in effect but not annotated. | **KEEP** | Before retiring: write the AR-001 rule into `architecture/assembly.md` "Local Files and Manifest" and the AR-002 rule into `architecture/evaluation.md` "Shared Executor" (both NOT COVERED), and annotate AR-003/004. AR-005..007 and RF-* are already mirrored in the holistic cross-reference. | none | high | 1f58d0f0 |

### 2.2 Deferred plans (open-work holders)

| Doc | Kind | Status | Disposition | What to do | Refs | Conf | Commit |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P `ConcurrentGarbageCollection_2026-08-28.md` | plan | deferred. The G4 entry gate is met, so the status line is stale. Open design gates 1–9 are all still open. | **KEEP** | Refresh the status. Fold holistic N2, E3, S4 and F5 into CG0. Restate the entry criterion, which cites a "dated post-integration review" that doesn't exist; point it at the G4 ADR. Retarget the Gate 9 link (`:667`) from the Regression plan to `architecture/evaluation.md:88-98`. Add Gate 9 hazard (b), the non-reentrant gate. | `architecture/evaluation.md:96-98,107-108` (textual) | high | df60695d |
| P `GarbageCollectorScopedPointerSafety_2026-09-09.md` | plan | deferred. G3 met; now waits on VRR. | **KEEP** (alternative: MERGE into VRR) | Drop the GCI5R-008 history from "Relationship". It holds unique alternatives, the SP0 "demonstrated defect" gate, and open naming decisions. | none | med-high | 6e3ffddb |
| P `ValueRepresentationRefinement_2026-08-19.md` | plan | deferred. Gate met, so the status is stale. | **KEEP** | Refresh the status. Add V-1 prework (holistic V1–V5) and "logical vs physical visits" to V0. Absorb the PNC columnar-descriptor item, shared-spine dedup and pressure tuning (§5.2). | `src/core/managed/value_node.rs:6` (by name) | high | 6e3ffddb |
| P `PureEffectAccessFusion_2026-09-23.md` | plan | deferred. Premise is stale (holistic R4: the W5B fusion loop is gone; `EFFECT_FUSION_BUDGET` is `#[cfg(test)]`). | **KEEP** | Rewrite Purpose, PEAF0 and PEAF1 so the baseline is the dispatch shortcut. Land R2 before PEAF1 and E4 before PEAF2. The oracle must be independent of the shared decoder. | none | high | d8d44e0c |
| P `PublicResumableEvaluation_2026-09-23.md` | plan | deferred. Status is stale: its "after W6G" gate was met on 2026-09-28. | **KEEP** | Refresh the status. Add PRE-0 prerequisites: S7 (the `advance_client_demand_for_test` duplicate at `session.rs:1143`) and A1 (hidden public tier). | none | high | daf9f147 |
| P `EvaluationRecursionPerformance_2026-10-04.md` | plan | deferred, current | **KEEP** | Add the W6G.4 O(depth) chain-walk evidence as prior art for hypothesis 1. Optionally re-check the G0 worker stack overflow (§5.2). | none | high | 1a2873a1 |
| P `ParserBacktrackingPerformance_2026-10-04.md` | plan | deferred, current. The fix is not implemented (`expression.rs:441-445`, `:566-569`). | **KEEP** | — | none | high | 1a2873a1 |

### 2.3 Collector crate and GC roadmap

| Doc | Kind | Status | Disposition | What to extract → destination | Refs | Conf | Commit |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P `GarbageCollectionRoadmap_2026-08-19.md` | plan | completed | **E-T-R** | Restate the 13 cross-plan invariants in `SAFETY.md` "Governing Invariants". Add a G0–G4 gate-scope table to a new `VERIFY.md` "Gate history". Move the "Explicitly Deferred" list to a new `SAFETY.md` "Non-goals and Deferred Work". Scope rationale → ADR. | **`SAFETY.md:1494` (blocking)** | med-high | 9e8d86fe |
| P `GarbageCollectorImplementation_2026-08-19.md` | plan | completed | **E-T-R** | Move rationale sentences into `SAFETY.md` sections and ADRs: C2A.1 geometry; the rejected C3B–C3D queued drain; clear-before-mark; chunk retention and decommit deferral; Trace-derive deferral; weak pointers as a non-goal. | `THIRD_PARTY.md:19` (by name); C-labels resolve in SAFETY/VERIFY | medium | 9e8d86fe |
| P `GarbageCollectionGateG0Baseline_2026-08-20.md` | plan | completed (measurement record) | **MERGE → `VERIFY.md` "Gate G0 Baseline"** | Environment, the 4-workload results, binary size, and the stack-overflow note. Fix the stale test name. | `VERIFY.md:983` | med-high | 807a91c1 |
| P `GarbageCollectorOwnershipLedger_2026-08-20.md` | plan | completed. No longer a test fixture (`74088606`). | **E-T-R** | M/R/C/T/E/D classification, the 8-field family-record schema, the `ManagedDropRecord` role, and one-write vs replaceable edges → `architecture/values.md`. Layout rows are latched in code; the verification matrix retires. | Code comments cite "the ownership ledger": `core/managed.rs:184`, `core/managed/value_node.rs:23`, `core/managed/recursive_cells.rs:1425` | medium | 8ad63790 |
| P `GarbageCollectorPublicValueAccessInventory_2026-08-28.md` | plan | superseded (status stale; the inventory model is now inverted in code) | **RETIRE** | — | none | high | 1a84ca39 |
| R `GarbageCollectorC2C_2026-08-22.md` | review | completed | **E-T-R** | RwLock rejection rationale → `SAFETY.md` "Regional Mutator Admission Invariants". Chunk decommit deferral (shared with Implementation). | none | high | 2d6eb2e4 |
| R `GarbageCollectorC6_2026-08-24.md` | review | completed | **E-T-R** | One sentence on the poison-vs-abort trade-off → `SAFETY.md` ~895-899 and an ADR. | none | high | bb205d9b |
| R `GarbageCollectorC8_2026-10-03.md` | review | completed | **MERGE → `VERIFY.md` "C8 collector measurements"** | Geometry table for 9 strides, the scan and finalization timings, and the paged-range reopen condition. | `VERIFY.md:1018`; holistic review `:26` | med-high | 9e8d86fe |
| R `GarbageCollectorGateG1_2026-08-25.md` | review | completed | **RETIRE** | Covered by `VERIFY.md:932-963` and `SAFETY.md:129-135`. The gate scope arrives through the Roadmap extraction. | `VERIFY.md:935`, `SAFETY.md:133` (repoint to the VERIFY section) | high | bb205d9b |

### 2.4 GC integration and the managed-value layer

| Doc | Kind | Status | Disposition | What to extract → destination | Refs | Conf | Commit |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P `GarbageCollectorIntegration_2026-08-19.md` | plan | completed. I11D.1 text describes the removed pre-entry aggressive hook. | **E-T-R** | Seed `architecture/values.md` (§3 outline): public Value contract, owner matrix, managed families, trace characteristics, the shell-conversion rule, opaque retention cost, three meanings of "purity", inline-arm scope. The "identical sample outputs across collection modes" rule → `agent_context/evaluation.md`. | I-labels: 40 non-inventory lines and 133 inventory lines; CG plan `:44-48` | medium | 9e8d86fe |
| R `GarbageCollectorIntegration_2026-08-25.md` | review | completed | **E-T-R** | GCI-002 "why public values have no equality" → values.md and an ADR. GCI-015's rejected "Automatic by default" → the NoAuto ADR. | none | high | 6aaaaeec |
| R `GarbageCollectorGateG2_2026-09-11.md` | review | completed | **RETIRE** | Family names and roles come through the ledger and plan extraction. The census is not durable. | none (the test `gate_g2_source_inventory_is_closed` is named after it) | high | 585cfec3 |
| R `GarbageCollectorGateG3_2026-10-02.md` | review | completed | **E-T-R** | "Collection is not a Glam semantic mutation" → values.md and `agent_context/evaluation.md`. The dynamic-tool boundary (Miri exclusion; sanitizers are not ordering evidence) → `AgentContext.md` Verification. | none | high | 26c86f71 |
| R `GarbageCollectorGateG4_2026-10-02.md` | review | completed | **E-T-R** | `ManagedDropRecord` rationale ("`Trace` does not constrain `Drop`") → ADR. Forward list → values.md "Deferred". | CG plan entry criterion | high | 8ad63790 |
| R `GarbageCollectorReadinessIntegration_2026-10-02.md` | review | completed. Its settlement clause is superseded. | **E-T-R** | Readiness precedence (Busy > MaintenanceFailed > MaintenanceRequired > coordinator) and advisory pressure vs authoritative request → values.md. Batch failure and poison policy → `architecture/assembly.md` "Batch Lifecycle". | none | high | 6aaaaeec |
| R `GarbageCollectorRuntimePolicy_2026-10-02.md` | review | completed. The primary record of the NoAuto decision. | **E-T-R** | Rationale and rejected alternatives → NoAuto ADR. The construction inventory (one constructor; no config selects the policy) → values.md. | none | high | cbfa4fa6 |
| R `GarbageCollectorExplicitMaintenance_2026-10-02.md` | review | completed. Its settlement clause is superseded. | **E-T-R** | Failure kinds `CollectorPanic`/`FinalizerPanic`, the `maintenance_failure` context, and "batch fails even after a successful retry" → `architecture/diagnostics.md` and `architecture/assembly.md`. Overhead invariant (the ordinary entry is a direct inline call) → values.md. | none | high | cbfa4fa6 |
| R `GarbageCollectorI13CleanupInventory_2026-10-02.md` | review | completed. Its "Gates and Adapters" text is superseded. | **E-T-R** | The Arc-role table, the provenance-and-identity-copies table and the drop-record decision → values.md. This is the best seed for defining "compatibility" (holistic X5/X8). | none | high | 06602491 |
| R `GarbageCollectorIntegrationI1_2026-08-28.md` | review | completed | **RETIRE** | Covered: `architecture/evaluation.md:60-69`, `agent_context/evaluation.md:10-24` | none | high | 2c54459c |
| R `GarbageCollectorIntegrationI2_2026-08-28.md` | review | completed | **RETIRE** | Covered: `architecture/evaluation.md:241-243`. The prototype was deleted. | none | high | 45635ead |
| R `GarbageCollectorIntegrationI3_2026-09-02.md` | review | completed. The gateway text is superseded. | **RETIRE** | Covered: `architecture/evaluation.md:193-199,262-268,346-351`, `SAFETY.md:1365-1372` | none | high | 5165f2bc |
| R `GarbageCollectorIntegrationI4_2026-09-03.md` | review | completed | **RETIRE** | Covered: `architecture/evaluation.md:241-243,257-261,479-495`, and a layout latch in code | none | high | 6c9581ef |
| R `GarbageCollectorIntegrationI5I10_2026-09-03.md` | review | completed | **E-T-R** | The acyclic-shell invariant → `agent_context/evaluation.md` "Values and Forcing". The no-weak-pointer rationale → ADR. The foreign-runtime resolver question → §5.2. | `I5F-003` in the recursive-identity inventory | med-high | 0840f7be |
| R `GarbageCollectorIntegrationI5_2026-09-07.md` | review | completed | **E-T-R** | A new-managed-family checklist → `agent_context/evaluation.md`. Rejected alternatives (fresh typestate, a temp root per allocation, recursive branding) → ADR. Rejected reflection-cycle fixes → ADR. | `GCI5R-*` in 4 inventories and `core.rs:300` | medium | d75adf0c |
| R `GarbageCollectorIntegrationI6I7_2026-09-10.md` | review | completed | **E-T-R** | Shared-spine logical-visit dedup is open future work → VRR plan. | none | high | 9d17b2bc |
| R `GarbageCollectorIntegrationI8_2026-09-10.md` | review | completed | **E-T-R** | Correct `agent_context/interaction_nets.md:117-122` (verified stale; code at `core_net.rs:133-144`). Per-agent allocation rationale → same section. | none | high | 335e37d6 |
| R `GarbageCollectorIntegrationI9_2026-09-11.md` | review | completed | **RETIRE** | Covered: `src/README.md:55`, `AgentContext.md:88-93` | none | high | 26a422d9 |
| R `GarbageCollectorIntegrationI10_2026-09-11.md` | review | completed | **E-T-R** | External-owner drain semantics and accepted conservative retention → `architecture/evaluation.md` ~346-356. | `I10A`/`I10B.0` labels in the containment inventory | high | 4da38497 |
| R `GarbageCollectorIntegrationI11_2026-09-11.md` | review | completed. Its aggressive mode is superseded. | **RETIRE** | Its one line ("GC timing never changes results") is shared with the G3 and I12 extractions. | `GCI11R` labels (owned by the Remediation plan) | med-high | 26c86f71 |
| R `GarbageCollectorIntegrationI12_2026-10-02.md` | review | completed | **E-T-R** | Race-safety reasoning, and "GC is unobservable to pure Glam" → `architecture/evaluation.md:100-108`. Its lease wording fixes `:103-104` (§5.1). | none | high | 242d47f1 |
| R `GarbageCollectorOpaqueRepresentation_2026-09-11.md` | review | completed (the I10B.0 decision record) | **E-T-R** | The rejected sealed managed arm, the reopen criteria, the accepted retention cycle and the `Arc` downcast contract → ADR and `architecture/evaluation.md` ~351. | the test `opaque_representation_review_inventory_is_complete`; `I10B.0` labels | high | c0015f1a |
| R `GarbageCollectorProductionCollectionI11B_2026-09-11.md` | review | completed | **RETIRE** | Covered: `architecture/evaluation.md:69-77,87-96,350-356` | none | high | 03aec8ca |
| R `GarbageCollectorWorkerFinalizationI11C_2026-09-11.md` | review | completed. Its hook placement is superseded. | **RETIRE** | Covered: `agent_context/evaluation.md:10-15,34-37`, `AgentContext.md:80-85` | none | high | 56968706 |
| R `GarbageCollectorRawValueApiAudit_2026-09-11.md` | review | completed (superseded baseline) | **E-T-R** | Lock-order rule (shared runtime-mutation admission may be taken inside a managed-access region) → `agent_context/evaluation.md:25-28`. | none | medium | 1b35d44c |

### 2.5 Aggressive verification and persistent edges

| Doc | Kind | Status | Disposition | What to extract → destination | Refs | Conf | Commit |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P `GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md` | plan | completed. Its "Existing Mode" text is false today. | **E-T-R** | The moving-GC retirement gate → ADR and values.md. The rejected `EvaluationHalt` continuation → `architecture/evaluation.md` "WHNF Submachine Flow". Probe/epoch fixture rules → `agent_context/evaluation.md` "Verification Discipline". The atom-root and traversal-separation decisions → "Collector Boundary". "No heap token in `Gc<T>`" → `SAFETY.md`. | `D.2*` labels (9 + 19 inventory lines); `GCI11R-002*` (~37) | medium | 26c86f71 |
| P `GarbageCollectorAggressiveVerificationRegression_2026-10-03.md` | plan | completed 2026-10-04 | **E-T-R** | D4 rejected fixes, the D6 livelock rationale and the non-reentrant gate invariant → `architecture/evaluation.md` "Collector Boundary". D9, D11 and the measurements → `AgentContext.md` "Verification". D8 → ADR. | CG plan `:667` (link) | med-high | 94ce5952 |
| P `GarbageCollectorPersistentEdgeTraits_2026-09-12.md` | plan | completed | **E-T-R** | Fix `SAFETY.md:271-274` and `:1346-1349` (verified stale against `pointer.rs:184`). "Roots gain no Eq/Ord/Hash" and the "no root per duplicate" rationale → `architecture/evaluation.md:336-344`. The 4 follow-ups → CG Gate 2, VRR and `SAFETY.md:639-647`. | **`architecture/evaluation.md:338` (link)**; `VERIFY.md:20` "P5B"; `SAFETY.md:1349` "P4"; ~4 `P4` code refs | high | 26c86f71 |
| R `GarbageCollectorAggressiveD2h_2026-10-01.md` | review | completed | **E-T-R** | The trait-removal partitioning lesson → `AgentContext.md` "Working Rules". The aggressive rerun triggers → `AgentContext.md` "Verification" (pending, §7 Q5). | none | high | 0a60ed34 |
| R `GarbageCollectorAggressiveVerificationClosure_2026-10-01.md` | review | completed. Its mode description is now false. | **E-T-R** | Two fixture rules (exact test roots are real ownership; reclamation fixtures assert liveness, not the reclaiming epoch) → `agent_context/evaluation.md` "Verification Discipline". | none | high | 26c86f71 |
| R `GarbageCollectorGCI11R002Holistic_2026-10-01.md` | review | completed | **RETIRE** | Its trigger list duplicates D2h; the sanitizer clause is covered by the G3 extraction. | none | high | 26c86f71 |
| R `GarbageCollectorI11D2DynamicToolMatrix_2026-10-01.md` | review | completed | **E-T-R** | → new `VERIFY.md` "Production-runtime dynamic matrix", and ideally `scripts/check.sh full`: the production-runtime Miri and sanitizer target list (one target name is stale), the Miri exclusion, and the Miri-only width 64. "`readiness()` is instantaneous" → `agent_context/evaluation.md`. Nightly warnings → §5.2. | none | high | c01e1437 |
| R `GarbageCollectorI11D2PersistentEdgeCost_2026-10-02.md` | review | completed | **RETIRE** | Covered: `VERIFY.md:15-21`, codegen script. The traffic fixture is self-describing in `pointer.rs:238`. | `VERIFY.md:20` (IDs only) | high | 26c86f71 |
| R `GarbageCollectorI11D3StaticClosure_2026-10-02.md` | review | completed | **RETIRE** | Covered: `SAFETY.md:383-460,928-949`. The readiness sentence goes with the DynamicToolMatrix extraction. | none | high | 26c86f71 |

### 2.6 Resumable WHNF

| Doc | Kind | Status | Disposition | What to extract → destination | Refs | Conf | Commit |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P `ResumableWhnfEvaluation_2026-09-12.md` | plan | completed (header `:9-12` stale) | **E-T-R** | Eight items (§3.5): the stack-safety rule; the delegation rule; the callable spill; lazy `map`/`concat` semantics; exact-route design; notification policy; hasher scope; and the rejected alternatives (per-source enums, declarative argument modes, per-session index). | W-labels: ~264 lines (~300 occurrences); 15 test files named `w*.rs` | med-high | 5b8a27f3 |
| P `ResumableWhnfHolisticReviewPlan_2026-09-28.md` | plan | completed (process scaffolding) | **RETIRE** | — | none (no inbound links at all) | high | 5b8a27f3 |
| R `ResumableWhnfHolistic_2026-09-28.md` | review | completed (WHNFHR-005 and 006 accepted or owned) | **E-T-R** (wave 5) | HR5 measurements and the Callgrind/DHAT recipe → `docs/Performance.md`. WHNFHR-004 ("`pump_until_stable` never claims foreground client demand") → `agent_context/evaluation.md` "Sessions and Workers". WHNFHR-006 reopen condition → CG plan or VRR. | none | medium | 5b8a27f3 |
| R `ResumableWhnfW3_2026-09-13.md` | review | completed | **RETIRE** | Covered: `agent_context/evaluation.md:75-83,101-105`, `architecture/evaluation.md:166-172` | none | high | 37775f5d |
| R `ResumableWhnfW4_2026-09-13.md` | review | completed | **RETIRE** | Its numbers are in the plan (`1882-1906`). | none | high | 66c2a3c4 |
| R `ResumableWhnfW4E_2026-09-14.md` | review | completed | **RETIRE** | Covered in code (`operator.rs:121-127`) and the plan (W4E.4). | `scripts/check-interaction-net-profiling.sh:7` ("W4E work fuse") | high | be4bedf4 |
| R `ResumableWhnfW5_2026-09-15.md` | review | completed | **RETIRE** | Its rejected alternatives are also in the plan (W5C.5a); they are extracted with the plan. | none | high | d8d44e0c |
| R `ResumableWhnfW6F_2026-09-18.md` | review | completed | **RETIRE** | Its negative result (per-session index) is also in the plan and extracted with it. | none | high | 05b7d368 |
| R `ResumableWhnfW6G1Baseline_2026-09-18.md` | review | completed (inventory, no numbers) | **RETIRE** | Superseded by W6G1Design and `architecture/evaluation.md:724-748` | none | high | c6288431 |
| R `ResumableWhnfW6G1Design_2026-09-19.md` | review | completed (unique design rationale) | **E-T-R** | The managed completion-promise design → **rewrite the stale** `architecture/evaluation.md:571-590,633`, `architecture/reflection.md:56-70` and `agent_context/evaluation.md:273-292`. The three ownership classes → `architecture/evaluation.md` "Lazy Producers". Plus an ADR. | none | high | d959cfd1 |
| R `ResumableWhnfW6GInterim_2026-09-21.md` | review | completed | **RETIRE** | Nothing unique. Its line 7 is a broken link. | none | high | d8d44e0c |
| R `ResumableWhnfW6GRemainingPlan_2026-09-21.md` | review | completed | **RETIRE** | Nothing unique. | none | high | d8d44e0c |
| R `ResumableWhnfW6G4_2026-09-23.md` | review | completed (unique measurements) | **E-T-R** (wave 5) | The debug, release, Callgrind and DHAT series, and the method (`perf` blocked → Valgrind; the net signature as oracle) → `docs/Performance.md`. The queued-before-stale selection rule and rejected alternatives → `architecture/evaluation.md` ~183-191. | holistic review `:281` | high | 8c611ae0 |
| R `ResumableWhnfW6G_2026-09-24.md` | review | completed | **RETIRE** | Covered: `architecture/evaluation.md:166-191,724-748`; PEAF | none | high | cd7773e9 |
| R `ResumableWhnfW7_2026-09-27.md` | review | completed | **E-T-R** | The stack-safety rule (shared with the plan) → `agent_context/evaluation.md`. Per-role FIFO requeue and worker task/spark alternation → `architecture/evaluation.md` "Shared Executor". | none | med-high | ca8548ca |
| R `ResumableWhnfW8_2026-09-27.md` | review | completed | **RETIRE** | Its invariant table duplicates HR1. W8R-002 and W8R-003 were resolved downstream. | none | high | 672845d5 |
| R `ResumableWhnfW9_2026-09-28.md` | review | completed | **RETIRE** | Its numbers are duplicated in plan W9E and holistic HR5. | none | high | 8c611ae0 |

### 2.7 Interaction-net callables and construction

| Doc | Kind | Status | Disposition | What to extract → destination | Refs | Conf | Commit |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P `InteractionNetCallableWhnfSpill_2026-09-16.md` | plan | completed | **E-T-R** | The `CallableCheckpoint` node contract, boxed-payload rationale and profiling-at-mutation-boundary rule → `agent_context/interaction_nets.md` "Core Specialization", replacing the stale `CallableData`/`HostFn` text. Deferred optimizations → §5.2. | NC-labels: 38 lines in 7 files; `eval/net/tests/nc5.rs`, `eval/whnf/tests/nc1.rs` | high | fd574a05 |
| R `InteractionNetCallableWhnfSpill_2026-09-16.md` | review | completed | **RETIRE** | Restates the plan. Its fixture-rooting lesson is covered in spirit by `AgentContext.md:37-40`. | none | high | fd574a05 |
| P `PureInteractionNetConstruction_2026-09-20.md` | plan | completed | **E-T-R** | → `agent_context/interaction_nets.md` "Templates and Construction": why `ListEffect`, the rejected reflection interpreter, search-order rules, and the replay one-shot caveat. Deferred performance → VRR (columnar) and §5.2. | PNC-labels: 54 lines in 9 files (22 are `reason =` in `core.rs`) | high | d492b966 |
| R `PureInteractionNetConstructionPNC3_2026-09-20.md` | review | completed | **E-T-R** | Quadratic indexed list-fix reevaluation is an unowned open item → §5.2. | `PNC3R-*` in 2 inventories | high | 8a2c8a2f |
| R `PureInteractionNetConstructionPNC4_2026-09-20.md` | review | completed | **E-T-R** | The `eval:{op:'net_construction}` frame rule → `agent_context/diagnostics.md` "Context Frames" (it lists only `net_computation`). | none | high | 87c4a3fa |
| R `PureInteractionNetConstructionPNC5_2026-09-21.md` | review | completed | **E-T-R** | → `interaction_nets.md` (together with the PNC plan): ambiguity is checked before exposure decoding; a third result is never demanded; a captured shift continuation is an ordinary `Value -> Effect`. | none | medium | 3bfc7def |

## 3. Extraction Details

Format for each extract: **source section** → **destination** (current
coverage), followed by drafted text where it is short. "NC" means not
covered: grep of the destination found nothing equivalent.

### 3.1 Implemented rules from the active docs: extract now

These rules are already in code. Only the active docs hold them, so they
need not wait for those docs to close.

| Rule | Source | Destination | Draft |
| --- | --- | --- | --- |
| Manifest write safety | Parallel review AR-001 (`:44-104`); `1f58d0f0` | `architecture/assembly.md` "Local Files and Manifest" (`CLI.md:95` only says the manifest cannot be an input) | "Manifest output snapshots the digest map and releases the lock before I/O. It rejects an output whose resolved identity (device and inode on Unix, canonical path elsewhere) matches a tracked input. It writes an exclusively created sibling temporary and renames it into place, so a destination symlink to an unrelated file is replaced, not written through." |
| Transactional worker activation | AR-002 (`:106-153`) | `architecture/evaluation.md` "Shared Executor" (NC) | "Worker activation is all-or-nothing. Workers spawn behind a start gate. On any spawn failure the executor aborts and joins the started workers, publishes nothing, and leaves activation retryable. On success it publishes handles, count and `executor_started`, then releases the workers." |
| Cause as nested frame | Holistic A3 (`:1463-1554`); `3f3ed946`, `f3b22303` | `agent_context/diagnostics.md` "Context Frames" (`:87-89` covers rendering only) | "- A failure that wraps another keeps the original diagnostic as a nested `msg` frame in its context. The headline states only the wrapper's own finding and never repeats the cause. Only a cause-less wait or unassigned promise keeps reason text in its headline." Also: in `CLI.md` ~376-384, a failing `conf.completion_script.NAME` fails the command with a `{conf:{entry:"completion_script"}}` frame. |
| Annotation ledger | Holistic E7 (`:926-952`) | `architecture/diagnostics.md` "Runtime Diagnostic Ingress" (NC); `agent_context/diagnostics.md:74` has the "library does not print" rule | "Unrecognized annotations are recorded in a deduplicated runtime ledger (a leaf lock, with no I/O). The assembler drains it after `eval` and `drain_reasoning` and publishes one `Warning` per annotation per runtime. `'deprecated` and `'TBD` take the same route." |
| Effect-task budget | Holistic R3 (`:1185-1194`) | `agent_context/reflection.md`, isolated-search bullets ~122-127 (NC; the general rule is at `agent_context/evaluation.md:359-367`) | "- `EffectTask::poll(steps)` spends one budget across the whole call and charges at least one step per pass. `pump_wait_on_route_within` charges the caller for each reserved quantum." |
| Poison and callback dispositions | Panic plan `:105-180`, `:255-310`, `:432-530` | `agent_context/evaluation.md` (locks); `agent_context/diagnostics.md` "Locking and Destruction"; `agent_context/reflection.md` (NC except runtime-core fault at `architecture/evaluation.md:551-557`) | "- Leaf locks recover poison with `into_inner`; each field documents why that is sound. Collector traces read through poison. Client values are dropped after glam locks are released. - Callbacks outside polls: commits and validations resume the client's own panic after releasing guards; runtime notifications skip the panicking callback; a panicking launcher interrupts only its own task." |
| Panic classes and fuzz policy | Panic plan `:536-558`, `:590-597` | `AgentContext.md` "Working Rules" and "Verification" (NC) | "Classify a panic site as I (internal invariant), U (user-reachable; convert to a diagnostic) or P (poisoning hazard). Fuzzing is a later discovery tool. Each finding becomes a deterministic regression, and no fuzz run joins `scripts/check.sh`." |

### 3.2 Collector crate and roadmap

**`GarbageCollectionRoadmap_2026-08-19.md`**
- "Cross-Plan Invariants" (`:153-284`) → `crates/glam-gc/SAFETY.md` "Governing Invariants" (`:1491-1496`), replacing the paragraph that points to the plan.
  - Coverage: invariants 1–3 and 6–13 are covered across SAFETY.md and the architecture docs; 4 is implicit; 5 and 12 are partial.
  - Draft:
    > The collector is exact, non-moving and stop-the-world, and runs a full collection. Each `EvaluationRuntime` owns one heap, and no managed edge crosses heaps. Roots are explicit; there is no stack scan. Finalization is passive and never takes a collector lock. Stable addresses are an implementation fact, not an API promise. Structural mutation goes through owner-qualified gateways. Opaque values are external handles. A failed collection is recoverable or permanently poisons the heap; it never retries a destructor on uncertain state. Unsupported layouts stay unsupported. This ledger restates only the representation facts needed to audit unsafe code.
- Gates G0–G4 (`:321-443`) → new `VERIFY.md` "Gate history" (NC as a set). Draft table:

  | Gate | Scope | Passed | Evidence |
  | --- | --- | --- | --- |
  | G0 | Pre-GC semantic regressions plus timing/RSS baseline | 2026-08-20 | "Gate G0 Baseline" |
  | G1 | Isolated collector certified | 2026-08-25 | "Gate G1 Certification" |
  | G2 | Production graph has no untraced edge or unclassified owner | 2026-09-11 | `docs/architecture/values.md` |
  | G3 | Forced collection over the whole production graph, including workers, finalizers, aggressive mode and dynamic tools | 2026-10-02 | "Production-runtime dynamic matrix" |
  | G4 | Integration I0–I13 closed; ownership classification final | 2026-10-02 | `Decisions.md` |

- "Explicitly Deferred" (`:482-511`), together with Implementation `:199-202` and `:4481-4483` → new `SAFETY.md` "Non-goals and Deferred Work" (NC). Draft:
  > The initial collector deliberately omits weak pointers and ephemerons; moving, generational and remembered-set collection; variable-size runs and a large-object path; chunk return or decommit before heap destruction; `Trace` derive macros; GC-aware persistent containers; cross-runtime migration; live-graph serialization; and stack maps. The concurrent-collector plan owns concurrent marking. The scoped-pointer plan owns lifetime-branded views.
- "Scope decision" (`:34-43`) → ADR "Exact, non-moving, stop-the-world collector". The "why" (reclaim recursive cycles such as fixpoints, not build a performance GC) is NC.
- Line 96 (logical tracing revisits shared spines) → values.md "Tracing" (NC; also holistic V2).

**`GarbageCollectorImplementation_2026-08-19.md`**
- C2A.1 geometry rationale (`:571-597`) → `SAFETY.md` "Run Topology Invariants". The values are covered at `:146-147` and `:182-183`; the rationale is NC. Draft:
  > The 64 KiB run, 8 MiB chunk and 128 B payload alignment were chosen against the I0 layouts (16–256 B): bitmap overhead is 0.11–1.55%, the maximum object is 65,408 B, the maximum alignment is 32 KiB, and a 1 B class costs about 20%.
- C3B–C3D queued-drain protocol, removed at C3E (`:1691-1823`) → `SAFETY.md` "Regional Mutator Admission Invariants" plus ADR. The mechanism is covered; the rejected alternative is NC.
- Clear-before-mark (`:2037-2045`) → `SAFETY.md` "Mark Attempt Invariants" (mechanism covered at `:198-200`). Draft:
  > Marks are cleared before marking; there is no mark colour, journal or allocation-time mark, which keeps the correctness surface small.
- Chunks are retained until heap destruction (`:3299-3302`) → `SAFETY.md` "Arena Ownership Invariants" (NC).
- Pressure ratio "to be measured in C8" (`:3271-3283`): never measured → §5.2.

**`GarbageCollectionGateG0Baseline_2026-08-20.md`** (MERGE)
- The whole results section → `VERIFY.md` "Gate G0 Baseline" (`:965-983`; method covered, numbers NC):
  - revision `60c6419e`, rustc 1.97.0;
  - 4 workloads, for example `direct_assembly_elf_w0` median 1,096 ms / 25,984 KiB RSS;
  - binary size 7,544,776 B.
- The known worker stack overflow (`:82-94`) → §5.2.
- Fix the stale test name: `public_value_factories_reject_foreign_composite_members` → `composite_construction_preserves_provenance_errors`.

**`GarbageCollectorOwnershipLedger_2026-08-20.md`**
- Classification M/R/C/T/E/D (`:32-72`) → values.md "Ownership Roles". Partly covered: `SAFETY.md:1076-1094` covers M and R, and `agent_context/evaluation.md:22-24` says roots never hide cycles. T, E and D are NC.
- Family-record schema and why `ManagedDropRecord` is mandatory (`:74-96`, `:461-463`) → values.md "Managed Families" (NC).
- One-write vs replaceable edges (`:312-348`) → values.md (mostly covered at `architecture/evaluation.md:356-368`).
- Repoint the 3 code comments to "`docs/architecture/values.md`, Managed Families".

**`GarbageCollectorC2C_2026-08-22.md`**
- GC2C-003 (`:208-215`) → `SAFETY.md` "Regional Mutator Admission Invariants". Draft:
  > Admission is a coordinator mutex and condition-variable state machine, not an `RwLock`: reader/writer priority is not a portable policy and cannot express dependent admission.
- GC2C-005 decommit deferral (`:333-336`): same destination as the Implementation chunk-retention sentence.

**`GarbageCollectorC6_2026-08-24.md`**
- GC6-002 resolution (`:182-192`) → `SAFETY.md` ~895-899 (partly covered). Draft:
  > Permanent poison was chosen over aborting the process: leaking Rust resources is preferable to retrying a destructor on uncertain state.

**`GarbageCollectorC8_2026-10-03.md`** (MERGE)
- Geometry table (`:52-62`), metrics scan (19.2 µs over 64 runs) and finalization structures (141 ns lookup; 100k finalizers in 13.3 ms) → `VERIFY.md` "C8 collector measurements" (`:985-1009`; the conclusions are covered, the numbers are NC).
- Paged-range reopen condition (`:110-114`). Draft:
  > Revisit range tracing only together with a real Glam-owned contiguous managed container and fresh measurements.

### 3.3 GC integration → `docs/architecture/values.md`

**Proposed `docs/architecture/values.md` outline (at most about 15 KB).** Line numbers refer to `GarbageCollectorIntegration_2026-08-19.md` unless another doc is named.

| # | Section | Sources |
| --- | --- | --- |
| 1 | Scope and glossary (owner lease, mutator, access region, publication nursery; the three meanings of "purity") | `:820-837` |
| 2 | Representation (inline-or-root; inline arm is `i64` only; one-slot typed-run limit) | `:381-433`, `:661-711`, `:4023-4045`, `:7146-7147` |
| 3 | Public Value contract (transport-only; observation needs authority; no Eq/Hash; content-free `Debug`) | `:713-781`, `:7142-7144`; 08-25 review GCI-002 `:109-159` |
| 4 | `CoreValueFactory` and `RuntimeValueDomain` (owner matrix; non-owning roots) | `:529-598`, `:631-657` |
| 5 | Access regions and allocation chronology | `:839-902`, `:2308-2350`, `:5163-5200` |
| 6 | Managed families (`ManagedFamily`, `ManagedDropRecord`, `managed_slot_extent`; family list) | `:600-629`, `:2372-2459`; ledger `:74-96`; G2 `:37-58` (names and roles only); I13 `:93-108` |
| 7 | Recursive identities (`recursive_cells`); link to `architecture/evaluation.md:317-334` | `:4550-4600`, `:4677-4980` |
| 8 | Tracing (`payload_edges`; logical, per-occurrence, non-forcing; cost proportional to references) | `:2544-2723`, `:4639-4676`, `:5788-5826`; Roadmap `:96` |
| 9 | What "compatibility" means; shell status; when to convert a shell | `:5202-5232`, `:5320-5387`, `:6996-7029`; I13 `:39-71` |
| 10 | Opaque and host-call boundary (`OpaquePayloadFamily`, external owners, accepted retention cycle) | `:469-475`, `:2461-2543`, `:5973-6283`, `:6047-6049`; opaque review |
| 11 | Runtime cache and durable roots (`runtime_cache`) | `:2724-2875` |
| 12 | Collection policy and maintenance (NoAuto; readiness precedence; failure policy; GC unobservable; CLI consequence that peak memory equals total allocation) | `:502-527`, `:6684-6987`; readiness review `:40-170`; RuntimePolicy `:17-124`; G3 `:132-147`; holistic `:296-301` |
| 13 | Invariants and verification | `:7099-7167`; G4 `:62-88` |
| 14 | Deferred work (links to the VRR, Concurrent GC and ScopedPointerSafety plans) | G4 `:178-196` |

The source docs below feed this outline. Each row lists only what is extra or goes elsewhere.

| Doc | Source → destination | Draft (where short) |
| --- | --- | --- |
| Integration plan | `:7130-7132` → `agent_context/evaluation.md` "Verification Discipline" (NC) | "- Sample outputs must be identical across collection modes; collection counts are profiling data, not semantics." |
| 08-25 review | GCI-002 `:109-159` → values.md §3 and ADR "Public Value is an opaque transport handle". GCI-015 `:1045-1058` → "Superseded decisions" table. | — |
| G3 | `:132-147` → values.md §12 and `agent_context/evaluation.md` "Values and Forcing" (partly covered: `architecture/evaluation.md:91-93`). `:121-130` → `AgentContext.md` "Verification" (NC). | "- Collection is not a Glam semantic mutation. It never advances observation epochs or changes values, transactions, diagnostics or net topology. Pure Glam cannot observe policy, pressure, revisions, counts or reports." / "Sanitizer passes are defect detectors, not evidence of concurrency ordering." |
| G4 | `:36-58` → ADR "Passive managed destruction with mandatory drop records". `:186-193` → values.md §14. | — |
| Readiness | `:120-129` and `:106-115` → values.md §12 (NC). `:141-170` → `architecture/assembly.md` "Batch Lifecycle" (NC; implemented at `bin/glam/batch.rs:222-236,271`). | "Readiness reports `Busy` while any GC lease is live, then `MaintenanceFailed`, then `MaintenanceRequired`, then the coordinator classification. Pressure is advisory; an explicit request is authoritative." / "Any maintenance failure fails the batch, even if a later service succeeds. A poisoned runtime yields a fallback host diagnostic and is dropped; the CLI never re-enters it." |
| RuntimePolicy | `:19-42`, `:92-124` → ADR "Runtimes never collect automatically". `:44-69` → values.md §12. The hello-world timing (`:71-90`) → `docs/Performance.md`. | — |
| ExplicitMaintenance | `:84-100` → `architecture/diagnostics.md` and `architecture/assembly.md` (NC). `:34-57` → values.md §12. | "Maintenance failures are `CollectorPanic` or `FinalizerPanic` and carry a `maintenance_failure` context frame." |
| I13 cleanup | `:39-71` → values.md §9. `:93-108` → values.md §6 and an ADR. | — |
| I5I10 | Acyclic-shell invariant → `agent_context/evaluation.md` "Values and Forcing" (NC). No-weak-pointer rationale → ADR. | "- With lazies, promises and nets removed, the remaining Rust-owned compatibility graph is acyclic. Never add interior mutability or recursion to an immutable shell." |
| I5 | Plan `:5163-5187` + GCI5R-001G.1 → `agent_context/evaluation.md` (NC). GCI5R-001/005 rejected alternatives → ADRs. GCI5R-004 → values.md §9. | "- Adding a managed family requires: a private allocator; no constructor that opens its own region and returns a fresh `Gc` or facade; publication before access ends; no root created only to bridge adjacent statements; a forced collection across the allocation/publication gap; exact trace, layout latch, passive drop with a `ManagedDropRecord`, and survival and reclamation tests." |
| I6I7 | Shared-spine visits are unoptimized → VRR plan V0 (NC in standing docs). | — |
| I8 | Replace `agent_context/interaction_nets.md:117-122` (verified stale). | "Core values carry the private `CoreRuntimeNet` facade: exactly one non-rooting managed edge. Inspection, mutation and root projection each need matching `RuntimeValueAccess`. Provenance is checked at public construction boundaries and by registered roots, not cached per edge. Runtime-net agents are ordinary allocations inside one managed cell; generic `SharedRuntimeNet` stays independent of `glam-gc`." |
| I10 | → `architecture/evaluation.md` ~346-356 (NC) | "External-owner drain detaches entries under the registry lock and drops them after unlock. A panicking destructor retires only its own owner, and untouched owners stay registered. Destructor order across concurrent drains is not semantic. Collection never retires an external owner; retention through embedder-supplied roots is an accepted conservative cost." |
| I12 | Race reasoning → `architecture/evaluation.md:100-108` (NC; latched by `managed_collection_tests.rs:1265-1279`). Also fix `:103-104`. | "Pressure is sampled under exclusive settlement admission. Each collection first publishes a GC lease under shared admission, and the stable predicate requires zero leases, so a race can cause one redundant attempt but never loses a request." |
| Opaque | Whole review (I10B.0 decision): rejected sealed arm, reopen criteria, retention → ADR `external-only-opaque-storage`. Downcast contract → `architecture/evaluation.md` ~351 (NC). | "An owning `Arc<T>` downcast of an opaque payload is sound only because opaque payloads are external owners." |
| RawValueApiAudit | → `agent_context/evaluation.md:25-28` (NC) | "- Shared runtime-mutation admission may be taken inside a managed-access region: settlement never enters the collector, and the collector elects exclusivity only after every mutator leaves." |

### 3.4 Aggressive verification and persistent edges

**`GarbageCollectorAggressiveVerificationRegression_2026-10-03.md`**
- D2 (`:126-159`) is covered at `architecture/evaluation.md:88-96`. Its rejected alternative (stop no-op leases from bumping the revision) goes to the ADR.
- D4's four rejected fixes, the D6 livelock rationale and counter definition, and the gate invariant → `architecture/evaluation.md` "Collector Boundary" (NC). Draft:
  > The runtime mutation-admission gate is a non-reentrant `RwLock`. Settlement holds it exclusively while constructing managed values, so no allocation path may take it. Aggressive pressure means "allocated since the previous evaluation" (`class_cache_hits + class_cache_misses`), not a constant `true`, which livelocks `settle_batch_runtime`.
- D9, D11 and the measurements → `AgentContext.md` "Verification" (NC). Draft:
  > Aggressive-GC verification collects about once per stable settlement cycle, not at every boundary; a test that needs a specific boundary requests collection explicitly. Tests whose primary purpose is NoAuto behaviour do not run under the feature; gate only the tail if the main purpose survives. Reference: aggressive library run 182 s; `scripts/check.sh full` 911 s (rustc 1.99.0, 8 threads, 2026-10-04).
- D8 → ADR and a `SAFETY.md` public-contract note: no pre-outer-entry collection hook exists.

**`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`**
- "Poll-spanning ownership and moving-GC retirement policy" (`:319-345`) → ADR and values.md §14 (NC).
- Rejected `EvaluationHalt` lazy/promise variants and outer restart (`:1398-1410`) → `architecture/evaluation.md` "WHNF Submachine Flow" (outcome covered at `:287-295`).
- Fixture rules 002E/002F (`:3061-3165`) → `agent_context/evaluation.md` "Verification Discipline" (partly covered at `:398-409`). Draft:
  > Install one-shot probes last. Snapshot completed epochs immediately before a disputed collection. Keep exact `+1` assertions inside the latched interval. Publish fixtures in one access region.
- Pre-closure decisions (`:2596-2636`) → "Collector Boundary". Draft:
  > Only the initial-metadata root is canonical; atoms are not rooted. Collector traversal is separate from mutator-qualified observation.
- Risk note → `SAFETY.md` "Managed Pointer and Access Invariants". Draft:
  > Do not add heap or domain tokens to `Gc<T>` or use a global allocation lookup to diagnose ownership.

**`GarbageCollectorPersistentEdgeTraits_2026-09-12.md`**
- Rewrite `SAFETY.md:271-274` and `:1346-1349` to state the closed contract: `persistent_edge_standard_trait_cutover_is_closed` at `pointer.rs:184`.
- `architecture/evaluation.md:336-344`: add "Root and public-value clones are cheap and gain no Eq, Ord or Hash. Duplicating an edge does not create a temporary root, which would be correct but far too expensive." Replace the `:338` link with a link to the ADR.
- Deferred follow-ups (`:833-845`):
  - destination-aware mutation writer → CG plan Gate 2;
  - mutable edge-slot discovery → ADR "Moving collection waits…";
  - revisit `Gc<T>` `Send`/`Sync` → `SAFETY.md:639-647`.

**`GarbageCollectorAggressiveD2h_2026-10-01.md`**
- D2HR-001 lesson → `AgentContext.md` "Working Rules" (NC). Draft:
  > Partition a cross-cutting trait or representation removal from the start by production seam, fixture seam, negative compiler contract and dynamic certification.
- D2HR-006 trigger list → `AgentContext.md` "Verification" (NC; pending §7 Q5).

**`GarbageCollectorAggressiveVerificationClosure_2026-10-01.md`**
- Fixture rules → `agent_context/evaluation.md` "Verification Discipline" (`:398-402` covers private domains only). Draft:
  > Test-only exact roots are real fixture ownership. Reclamation fixtures assert liveness and eventual collection, not which epoch reclaimed a slot.

**`GarbageCollectorI11D2DynamicToolMatrix_2026-10-01.md`**
- "Selected Target Matrix" and I11D.2b → new `VERIFY.md` "Production-runtime dynamic matrix" (NC; `scripts/check.sh full` runs only glam-gc Miri and sanitizers):
  - Miri targets plus sanitizer filters `core::managed::value_node::tests::prepared_root`, `core::managed::tests::runtime_value_access` and `api::tests::managed_collection_tests::`;
  - Exclusion: `production_collection_preserves_each_serial_boundary` exceeds 15 CPU-minutes under Miri, so it is a performance exclusion, not a pass;
  - Miri-only width 64 for `checked_nonrecursive_marking_handles_wide_shared_spines`;
  - Rename the stale `repository_aggressive_mode_enables_each_production_runtime`.
- "`Runtime::readiness()` is an instantaneous observational probe that may race a worker parking" → `agent_context/evaluation.md` "Verification Discipline" (NC).

### 3.5 Resumable WHNF

**`ResumableWhnfEvaluation_2026-09-12.md`** (the eight items)

| Item | Source | Destination (coverage) | Draft |
| --- | --- | --- | --- |
| Stack rule | W7A.1 `:8063-8094`, invariant 18; W7 review | `agent_context/evaluation.md` "Values and Forcing" (NC; only fixpoints at `:235`) | "- No user-controlled semantic recursion on the Rust stack. Allowed: bounded plumbing, log-depth balanced containers, and owned worklists such as cursor WHNF. Deep-structure tests use explicit depths and a small-stack control that must report a stack overflow." |
| Delegation rule | Invariants 4 and 9; `:9409-9438` | same (NC) | "- WHNF delegation creates no lazy, root or allocation. Root registration that scales with semantic depth is a regression." |
| Callable spill | W6B.4b.2 `:3930-3945` | `agent_context/interaction_nets.md` (NC); see NC plan | — |
| Lazy `map` and `concat` | W6D.4a/b `:4204-4300` | `agent_context/evaluation.md` "Values and Forcing" (NC); maintainer may want `Design.md` | "- `map` and `list.concat` are structural and non-forcing. `map f (A ++ B)` yields deferred halves, strict leaves are balanced to O(log n) depth, and an invalid item becomes a deferred failing hole." |
| Exact route | W9C.3 `:8959-9050` | `architecture/evaluation.md` "Scheduler State Ownership" (`:183-191` partial) | "Foreground demand keeps a caller-local route as a validated hint. A broad `work_generation` plus a private route-hazard revision decide whether it is still valid; cold traversal remains the authority." |
| Notification | W9C.4/W9D.4 `:9135-9184`, `:9288-9313` | `agent_context/evaluation.md` "Sessions and Workers" (NC) | "- The coordinator condvar serves several waiter classes, so never use `notify_one`. Wakes may be suppressed only for non-enabling mutation kinds; the work generation always advances." |
| Hasher scope | W6G4R-002 `:7759-7766` | same (NC; code comment `coordinator.rs:112-118`) | "- Deterministic hashers only for trusted private-ID sets; indexes keyed by user data, and persistent indexes, stay randomized." |
| Rejected alternatives | W0B/W0C `:152-156`, `:515-575` (per-source phase enums); W5C.5a `:3308-3320` (Raw/Whnf flags, declarative prep, synchronous evaluation); W4E.3 `:2132-2140` (per-session index: no gain) | ADRs "Resumable WHNF…" and "Pollable specialization requests"; one sentence in `architecture/evaluation.md:166-172` | "A per-session running-machine index was measured and gave no gain; it was reverted." |

Also: fix the stale header sentence at `:9-12` before retiring, or skip it if the
doc is deleted. The `.read.token` non-suspending nested search (`:3262-3266`)
goes to §5.2.

**`ResumableWhnfHolistic_2026-09-28.md`** (wave 5)
- HR5 measurements and recipe (`:614-660`) → `docs/Performance.md` (NC; holistic X3 flags this).
- WHNFHR-004 (`:1105-1128`) → `agent_context/evaluation.md` "Sessions and Workers" (NC). Draft:
  > `pump_until_stable` never claims foreground client demand.
- WHNFHR-006 route-storage reopen condition (`:1163-1188`) → CG plan next to `:282-287`.

**`ResumableWhnfW6G1Design_2026-09-19.md`**
- Rewrite the stale sidecar text, verified at `architecture/evaluation.md:571-590` ("stable task observation") and `:633`, `architecture/reflection.md:56-70` and `agent_context/evaluation.md:273-292`. Code: `core.rs:1821` `completion_promise`. Draft:
  > An activated reflection task runs to terminal and fulfills a managed completion promise. A terminal mapper covers the return value, the gate target and every abnormal outcome. The lazy's WHNF checkpoint waits on that promise. Last-subscriber retirement never cancels the task. An unobserved failure stays in the runtime ledger.
- Three ownership classes (demand-driven managed checkpoints; autonomous root work; orchestration after access closes) → `architecture/evaluation.md` "Lazy Producers" (NC).

**`ResumableWhnfW6G4_2026-09-23.md`** (wave 5)
- Timing, Callgrind and DHAT series, and the method → `docs/Performance.md` (NC). Draft of the method:
  > `perf` is unavailable in the container (`perf_event_paranoid=4`); use Valgrind Callgrind and DHAT. Wall time is corroboration only; the interaction-net work signature is the semantic oracle.
- Selection rule → `architecture/evaluation.md` ~183-191 (NC). Draft:
  > A queued ancestor runs before its stale block is followed; foreground and background selection share this rule.
- Rejected alternatives (eager root-to-leaf cache; authoritative ready-descendant index; hasher as primary repair) → the route ADR.

**`ResumableWhnfW7_2026-09-27.md`**
- Stack rule: shared with the plan above.
- Fairness → `architecture/evaluation.md` "Shared Executor" (NC; code `coordinator.rs:3776-3812`). Draft:
  > Each role requeues FIFO; workers alternate between tasks and sparks.

### 3.6 Interaction nets

**`InteractionNetCallableWhnfSpill_2026-09-16.md`**
- Topology (`:213-225`), trait contract (`:227-257`) and sizing (NC0B `:585-596`) → `agent_context/interaction_nets.md` "Core Specialization". That section's `:284-317` is stale (`CallableData`, `HostFn`, `Error` operator, cloneable `Data`). Draft:
  > `CallableCheckpoint(NetWhnfState)` is a one-port, linear, runtime-only node. Its only rule is with `Bind`; Fan, Erase and every other partner are stuck. It never appears in a template and is never copied. Callable WHNF runs inline first. Only budget exhaustion or a real dependency installs a checkpoint. The payload is the canonical boxed `WhnfState` (`Send + 'static` only) so the node stays small.
- Profiling counters commit at the mutation boundary (`:1299-1305`) → `interaction_nets.md` "Reduction and External Work" (NC).
- §Deferred Optimization (`:1407-1419`) → §5.2.

**`PureInteractionNetConstruction_2026-09-20.md`** and the **PNC5** review
- "Why ListEffect" and the rejected reflection interpreter (`:53-101`; PNC0 measured route-loss replay) → `interaction_nets.md` "Templates and Construction" (NC). The architecture itself is covered at `:16-24` and `:55-81`.
- Search order (`:136-138`, `:269-284`; PNC5 `:41-76`) → same (NC). Draft:
  > A blocked left alternative blocks the whole search. The second-result check stays lazy. Ambiguity is decided before exposure decoding, and a third result is never demanded. A captured shift continuation is an ordinary `Value -> Effect`.
- Replay caveat (`:1093-1096`) → same (`:59-62` covers synchronous replay). Draft:
  > If replay ever gains a yield or callback boundary, replace the one-shot proof.
- §Deferred Performance Work (`:1143-1157`) → VRR (columnar descriptors) and §5.2.

**`PureInteractionNetConstructionPNC3_2026-09-20.md`**: the quadratic indexed list-fix reevaluation (`:116-119`) → §5.2.

**`PureInteractionNetConstructionPNC4_2026-09-20.md`**: PNC4R-001 (`:70-114`, `:225-241`) → `agent_context/diagnostics.md` "Context Frames" (NC; `:33-34` lists only `net_computation`). Draft:
> - Interaction-net construction adds one `eval:{op:'net_construction}` frame around the whole public pipeline. Private operand demands (copy counts, wire ports, reset/shift keys, paths, state) add none.

## 4. Draft ADRs

Proposed home: `docs/Decisions.md` (§1.3). Each heading is descriptive, and
the `slug` doubles as its anchor. "Agent" means an agent decided it and
recorded the decision in a plan or review, without naming the maintainer.
"Maintainer" means a doc or commit attributes the decision to the
maintainer.

Totals:
- 53 entries: 46 accepted, 6 pending a maintainer decision, 1 proposed.
- 6 superseded decisions, recorded in §4.9.
- 9 of the accepted entries name the maintainer.

### 4.1 Process and verification

**Panics are bugs and task-layer interruptions, never semantics** · `panics-are-task-interruptions`
- **2026-10-04 · maintainer · accepted.**
- **Context:** two crashes were reproduced (a net and a parser case), and scheduler claims had no unwind containment.
- **Decision:**
  - Code that observes user input detects invalid states first. Evaluation reports `EvaluationFailure`; parsing backtracks and keeps closest-match diagnostics.
  - A panic is never an `EvaluationFailure`, a cached lazy result, `.alt`/`.fail` input, or a diagnostic.
  - Placement: lazies get a `Panicked` evaluation state; tasks get a `Panicked` wait terminal. A cached-result placement was rejected.
- **Consequences:**
  - Only the poll boundary creates a panic value, and waiters halt.
  - Public surface: `ErrorKind::Panic` and `.task.status` `panicked`.
  - There is no cost when nothing panics.
- **Source:** holistic X4, Decision 1; panic plan `:15-28`, `:182-253`. **Coverage:** `AgentContext.md:66-74`. The placement rationale is NC.

**Poison recovery follows lock class; runtime-core poison faults the runtime** · `poison-recovery-by-lock-class`
- **2026-10-04 · maintainer · accepted.**
- **Context:** a caught panic stranded claims, cascaded poison into aborts, and wedged GC.
- **Decision:**
  - Leaf locks recover with `into_inner`; collector traces read through poison.
  - A poisoned value cell puts its lazy into `Panicked`.
  - Poison in the runtime core sets one fault flag: readiness becomes `Poisoned`, there is no abort and no hang, and destructors become no-ops.
  - Client callbacks are handled by kind: resume for commits and validations, skip for notifications, and interrupt only the launcher's own task.
- **Consequences:** a panic in read-only core code is detected lazily (an accepted gap). Making the core panic-free is deferred.
- **Source:** panic plan decision 2, `:105-180`, `:255-310`, `:432-530`. **Coverage:** `architecture/evaluation.md:551-557` covers the core fault only.

**Discover panics by inspection; fuzzing is deferred and never gates** · `panic-discovery-by-inspection`
- **2026-10-04 · maintainer · accepted.**
- **Context:** the holistic review's P0-2 asked for a front-end no-panic fuzz target.
- **Decision:**
  - Inspect risk-first, using site classes I (internal invariant), U (user-reachable) and P (poisoning hazard).
  - Fuzz with `cargo-fuzz` only when inspection stalls.
  - Every finding becomes a deterministic regression, and no fuzz run joins `scripts/check.sh`.
- **Consequences:** this supersedes the P0-2 fuzz target (§4.9). The polarity plan's slice 6 follows this policy.
- **Source:** panic plan `:536-558`, `:590-597`; commit `64092e4b`. **Coverage:** NC.

**Temporary negative tests during transitions; retire most inventories** · `transition-negative-tests-not-inventories`
- **2026-10-03 · maintainer · accepted.** The surviving set is pending.
- **Context:** about 12.8k lines of source-scanning inventories with exact counts and fingerprints tax every structural change.
- **Decision:**
  - Retire most inventories.
  - During a major transition, add syntax-backed negative tests keyed by module and item. Retire them at the closing review, keeping only durable rules.
  - Census totals and fingerprints are never evidence of safety.
- **Consequences:** this supersedes the 2026-10-01 rule "keep exact inventories through G3" and RF-003's "share inventory parsing" (§4.9).
- **Source:** holistic X5, V4, Decision 4. **Coverage:** `AgentContext.md:86-93`.

**Rust code never cites plan or review prose** · `no-code-references-to-history-docs`
- **2026-10-03 · agent (X1/AR-003) · accepted.**
- **Context:** a unit test failed because a plan's wording changed.
- **Decision:** `tests/source_doc_coupling.rs` fails if `src/**/*.rs` names `docs/plans` or `docs/reviews`.
- **Consequences:** history docs can be edited or deleted freely. Labels remain a soft coupling (§6). The guard scans `src/` only.
- **Source:** holistic X1. **Coverage:** `AgentContext.md:93`.

**Pinned toolchain and three cumulative check levels; no CI yet** · `pinned-toolchain-and-check-levels`
- **2026-10-03 · decider not stated (commits `4c849c19`, `5e68163d`) · accepted.**
- **Context:** the routine checks were red and incomplete, and toolchains differed between reviewers.
- **Decision:**
  - `rust-toolchain.toml` pins 1.99.0, matched by `rust-version`. Upgrade only in a dedicated plan-boundary commit.
  - `scripts/check.sh` has three levels: `fast`, `all` (the pre-commit gate) and `full` (periodic).
- **Consequences:** Miri and the sanitizers run only when a nightly is installed.
- **Source:** holistic X2; AR-004. **Coverage:** `AgentContext.md:97-126`.

**Aggressive-GC rerun triggers** · `aggressive-gc-rerun-triggers`
- **2026-10-01 · agent · PENDING.**
- **Context:** aggressive mode stayed red from I12A until 2026-10-03 because it runs only "periodically".
- **Proposal:** rerun `full` after changes to runtime or collector code, or to unsafe, tracing, mutation, root, admission, finalization or scheduler behaviour. Documentation-only changes may reuse the last result.
- **Source:** D2h D2HR-006; GCI11R002 holistic GCI2HR-006. **Coverage:** NC (`AgentContext.md:115-118` says "periodically"). See §7 Q5.

**Retired plans and reviews are deleted, not archived** · `retired-history-is-deleted`
- **PENDING (holistic Decision 5); recommended.**
- **Decision:** delete a history doc once three things hold: its durable decisions live in standing docs or `Decisions.md`; nothing current references it; and its last commit is listed in `plans/README.md`.
- **Consequences:** recovery is through `git show`. This matches the earlier deletions (`233fde61`, `d35d899e`, `4e1d7925`).
- **Source:** holistic X8 `:601-605`; §1.3.

### 4.2 Language and front end

**Language declarations fail fast; source is ASCII unless `utf8`** · `fail-fast-language-declaration`
- **2026-10-05 · maintainer · accepted.**
- **Context:** `language g9 with nonsense` compiled and ran.
- **Decision:**
  - A base other than `g0`, or an extension other than `utf8`, is an error that stops parsing.
  - Without `utf8`, the first non-ASCII character anywhere, texts and comments included, is an error.
- **Consequences:**
  - Compile, inspect and test paths all admit the declaration.
  - The `demo` extension tests were inverted, and invalid samples were added.
- **Source:** holistic F7, Decision 8; commit `8853d4e8`. **Coverage:** `agent_context/g_syntax.md:16-21`; `architecture/front_end.md:46-47`.

**`map` and `list.concat` are structural and non-forcing** · `structural-lazy-map-and-concat`
- **2026-09-17 · agent · accepted.** This is user-visible semantics; see §7 Q4.
- **Context:** recursive list operators forced whole spines on the Rust stack.
- **Decision:**
  - `map f (A ++ B)` yields deferred halves.
  - Strict leaves are balanced to O(log n) depth.
  - Invalid items become deferred failing holes.
- **Consequences:** laziness is observable. A failure appears only when the item is demanded.
- **Source:** resumable-WHNF plan W6D.4a/b `:4204-4300`. **Coverage:** NC.

**Script extension handling** · `script-extension-handling`
- **PENDING (holistic Decision 7).**
- **Question:** reject unknown `--script.EXT` extensions, or correct `CLI.md:39`, which claims the extension selects the compiler?
- **Source:** holistic A5.

### 4.3 Diagnostics

**A failure's cause is a nested `msg` frame; the headline states only its own finding** · `failure-cause-as-nested-msg-frame`
- **2026-10-05 · maintainer (headline rule) and agent (implementation) · accepted.**
- **Context:** macro, `conf.cli`, killed-work and logger failures were flattened to text, and one `.ok()` swallowed a configured error.
- **Decision:**
  - Keep the original diagnostic as a nested context message.
  - The headline never repeats the cause.
  - A `conf.completion_script.NAME` failure fails the command with a `{conf:{entry}}` frame.
  - `.task.status` reports `killed:Diagnostic`, stored un-normalized.
- **Consequences:** genuinely new validation errors stay as text.
- **Source:** holistic A3; commits `4cceabbc`, `3f3ed946`, `f3b22303`. **Coverage:** `agent_context/diagnostics.md:9-11,31,87-89`. The cause/headline rule is NC (§3.1).

**Unrecognized annotations go through a runtime ledger; the library never writes stderr** · `annotation-warnings-via-runtime-ledger`
- **2026-10-05 · maintainer (chose the runtime ledger) · accepted.**
- **Context:** the evaluator wrote to stderr directly.
- **Decision:**
  - A deduplicated leaf-lock ledger with no I/O.
  - The assembler drains it and publishes one `Warning` per annotation per runtime.
- **Consequences:** `'deprecated` and `'TBD` share the route.
- **Source:** holistic E7; commit `d701396d`. **Coverage:** the rule only, at `agent_context/diagnostics.md:74`. The mechanism is NC.

**Rust `Display` of evaluation failures is an edge-free classification** · `failure-display-is-edge-free`
- **2026-09-30 · agent · accepted.**
- **Context:** `Display` cannot carry a mutator, so it cannot observe managed values.
- **Decision:** `EvaluationFailure::Display` prints a classification with no cached eager summary.
- **Consequences:** rendering goes through the configured logger.
- **Source:** aggressive-remediation plan, D.2h pre-closure (`:2596-2636`); D2h D2HR-003. **Coverage:** `architecture/diagnostics.md:19-21`.

**One public frame for interaction-net construction** · `single-net-construction-frame`
- **2026-09-21 · agent · accepted.**
- **Context:** builder operands added redundant frames.
- **Decision:**
  - One `eval:{op:'net_construction}` frame around the public pipeline.
  - Private operand demands stay transparent.
  - The legacy `copy_count` frame is dropped.
- **Consequences:** immediate validation strings are not compatibility promises.
- **Source:** PNC4 review PNC4R-001. **Coverage:** partly `Syntax.md:1078-1081`. NC in `agent_context/diagnostics.md`.

### 4.4 Interaction nets

**Interaction nets are polarized: `Bind >< Bind` joins crossed; the exposed port is `+`** · `polarized-interaction-nets`
- **2026-10-05 · maintainer · accepted.** Enforcement is pending in polarity slices 2–4.
- **Context:** N8 fuzzing needs well-formed nets, and the bind join was positional.
- **Decision:**
  - Every wire joins `+` to `−`.
  - A function bind lists `[result, argument]` and an application `[argument, result]`; they join crossed.
  - The exposed port is `+` by fiat.
  - A constructed component unreachable from the exposed port is an error; reduction garbage is allowed.
  - Polarity is a construction contract and is not stored on nodes.
- **Consequences:**
  - A linear checker runs at `try_finish`, with a `NetBuildError::Polarity`.
  - Rewrites must preserve polarity.
  - GAL levels are deferred.
- **Source:** polarity plan `:25-76`; holistic N8; commits `d5cf8c6f`, `13082fe1`. **Coverage:** `agent_context/interaction_nets.md:37-39,83-113`; `Design.md`.

**Callable WHNF runs inline first and spills to a linear checkpoint node only on suspension** · `inline-first-callable-spill`
- **2026-09-16 · agent · accepted.**
- **Context:** `Bind >< Data` forced callables synchronously.
- **Decision:**
  - Run inline first.
  - Only budget exhaustion or a real dependency installs `CallableCheckpoint(NetWhnfState)`, a one-port linear node whose only rule is with `Bind`.
  - An `Operator >< Data` encoding and `Gc<WhnfState>` were rejected.
- **Consequences:** there is no side table and no semantic value variant, and the payload is boxed.
- **Source:** callable-spill plan `:3-6`, `:213-257`, `:373-389`; resumable-WHNF plan W6B.4b.2. **Coverage:** NC (§3.6).

**Net construction is pure state over ordered `ListEffect` search plus hidden strict-netlist replay** · `pure-listeffect-net-construction`
- **2026-09-20 · agent · accepted.**
- **Context:** the reflection-task interpreter was root-heavy and replayed routes.
- **Decision:**
  - A pure builder.
  - First-two selection.
  - A hidden `InteractionNetFromNetlist` replay.
  - No reflection, heap, task, log or env access.
- **Consequences:** replay is synchronous with a one-shot proof. Large-netlist budgeting is deferred.
- **Source:** pure-construction plan `:53-101`, `:155-310`. **Coverage:** the mechanism at `agent_context/interaction_nets.md:16-24,55-81`. The rationale is NC.

### 4.5 Evaluation and scheduling

**Resumable WHNF through bounded regional quanta and durable checkpoints** · `resumable-whnf-regional-quanta`
- **2026-09-12 · agent · accepted.**
- **Context:** the recursive evaluator replayed prefixes after retryable halts, and reflection decoding created unbounded fresh lazies.
- **Decision:**
  - One `WhnfComputation` with a shared explicit work stack and a nine-variant vocabulary.
  - Each quantum is callback-free under access.
  - A durable checkpoint is taken only at real boundaries.
  - Per-source phase enums were rejected: the census found 157 demand-then-inspect sites against 2 tail demands.
- **Consequences:** no replay and a small Rust stack, at the cost of an edge walk per quantum.
- **Source:** resumable-WHNF plan `:93-288`, W0B/W0C. **Coverage:** `architecture/evaluation.md:270-295`. The rejection is NC.

**Partial lazy production belongs to the lazy; `LazySource` is an immutable recipe** · `lazy-owns-partial-progress`
- **2026-09-21 · agent · accepted.**
- **Context:** session-owned progress kept cycles alive.
- **Decision:** partial progress lives beneath the owning lazy, and coordinator routes are session-neutral.
- **Consequences:** cycles stay collectible.
- **Source:** resumable-WHNF plan W6G.1c `:5035-5068`. **Coverage:** `architecture/evaluation.md:735-748`.

**Autonomous reflection tasks publish through a managed completion promise** · `reflection-tasks-publish-via-completion-promise`
- **2026-09-19 · agent · accepted.**
- **Context:** reflection had been misclassified as a lazy checkpoint, with an external task-observation sidecar.
- **Decision:**
  - The activated task runs to terminal.
  - A terminal mapper fulfills a managed completion promise, on which the lazy's checkpoint waits.
  - Last-subscriber retirement never cancels the task.
  - A spark needs only at-most-once scalar admission.
- **Consequences:** the sidecar was removed. **Standing docs still describe it** (§5.1).
- **Source:** W6G1Design review; resumable-WHNF plan `:5986-6080`. **Coverage:** NC (stale).

**Specialization callbacks use pollable request work** · `pollable-specialization-requests`
- **2026-09-14 · agent · accepted.**
- **Context:** synchronous evaluation inside request preparation forced replay.
- **Decision:**
  - Request work is specialization-owned and pollable.
  - `RequestContext` loses evaluation.
  - Rejected: per-argument Raw/Whnf flags, declarative preparation with a second continuation, and a universal reflection continuation.
- **Consequences:** a source-breaking change to `TaskSpecialization`.
- **Source:** resumable-WHNF plan W5C.5a `:3208-3320`; W5 review. **Coverage:** `architecture/reflection.md:147-157`. The rejections are NC.

**No user-controlled semantic recursion on the Rust stack** · `no-semantic-recursion-on-rust-stack`
- **2026-09-24 · agent · accepted.**
- **Context:** stack overflow on deep user data.
- **Decision:** the only permitted forms are bounded plumbing, log-depth balanced containers, and owned worklists.
- **Consequences:** small-stack controls in tests. The remaining gaps (deep parse nesting, core drop) are in the panic plan's known gaps and holistic V3.
- **Source:** resumable-WHNF plan W7A.1 `:8063-8094`; W7 review. **Coverage:** NC.

**The step budget is a reservation; a poll spends at most its budget** · `step-budget-is-reservation`
- **2026-09-25 (W7C.0), extended 2026-10-05 (R3) · agent · accepted.**
- **Context:** exact spend and foreground allowance were conflated, and isolated search squared its budget.
- **Decision:**
  - `EvaluationStepBudget` is exact.
  - Foreground reservation is an allowance.
  - `EffectTask::poll` keeps one budget per call.
- **Consequences:** `Yielded` still conflates progress and budget; holistic E1 is open.
- **Source:** resumable-WHNF plan `:8228-8245`; holistic R3. **Coverage:** `architecture/evaluation.md:221-233`; R3 is NC.

**Foreground demand keeps a validated route hint; full traversal stays authoritative** · `validated-exact-route-hint`
- **2026-09-23 to 2026-09-28 · agent · accepted.**
- **Context:** the W6G.4 profile showed 18,800 chain walks over 2.76M edges.
- **Decision:**
  - A caller-local route checked by `work_generation` plus a private hazard revision.
  - A queued ancestor runs before its stale block is followed.
  - Rejected: an eager root-to-leaf cache, an authoritative ready-descendant index, and a hasher as the primary repair.
- **Consequences:** −23% instructions. Route depth is still O(depth), an accepted cost (WHNFHR-006).
- **Source:** W6G4 review; resumable-WHNF plan W9C.3. **Coverage:** partly `architecture/evaluation.md:183-191`.

**Shared-condvar notification policy** · `shared-condvar-notification-policy`
- **2026-09-28 · agent · accepted.**
- **Context:** `notify_one` caused a real spark/client admission bug.
- **Decision:** never use `notify_one` on the shared condvar. Suppress only the non-enabling mutation kinds. The generation always advances.
- **Consequences:** −33.6% `notify_all`. Splitting the condvar is deferred until a profile with workers enabled shows a need.
- **Source:** resumable-WHNF plan W9C.4/W9D.4. **Coverage:** NC.

**A `TaskHalt` is only a failure or a panic** · `taskhalt-is-failure-or-panic`
- **2026-10-05 · agent · accepted.**
- **Context:** a leftover `TaskHalt::Blocked` translation.
- **Decision:** the `EvaluationHalt`→`TaskHalt` conversion is removed. Waits become `WorkDependency::Wait`.
- **Consequences:** reflection has one halt vocabulary.
- **Source:** holistic R7; commit `6215f517`. **Coverage:** `agent_context/reflection.md:15-26`.

**Worker activation is all-or-nothing and retryable** · `transactional-worker-activation`
- **2026-10-05 · agent · accepted.**
- **Context:** a spawn failure part-way through left workers live and retry rejected.
- **Decision:**
  - Workers spawn behind a start gate.
  - On failure: abort, join, publish nothing.
  - On success: publish, then release.
- **Consequences:** there is a test-only spawn-failure hook.
- **Source:** AR-002; commit `1f58d0f0`. **Coverage:** NC (§3.1).

**Inline-first lazy forcing** · `inline-first-lazy-forcing`
- **PENDING (holistic Decision 3).**
- **Question:** should an uncached lazy be forced inline, changing the rule that every uncached lazy is a coordinator route?
- **Source:** holistic S1, E2.

**Reflection: `.heap.get` outside a cut, and continuation lifetime** · `reflection-heap-get-and-continuation-lifetime`
- **PENDING (holistic Decision 6).**
- **Question:** is `.heap.get` outside a cut retry-observable, and how long do captured continuations live?
- **Source:** holistic R6, R8.

### 4.6 Assembly

**Manifest writes are identity-checked and atomically published** · `atomic-identity-checked-manifest`
- **2026-10-05 · agent · accepted.**
- **Context:** an output aliasing an input through a symlink or hard link truncated it.
- **Decision:** reject a matching file identity, then write a sibling temporary and rename it into place.
- **Consequences:** a symlinked destination is replaced, not written through.
- **Source:** AR-001; commit `1f58d0f0`. **Coverage:** NC (§3.1).

### 4.7 Managed values and collection policy

**The public `Value` is an opaque transport handle** · `public-value-is-opaque-handle`
- **2026-08-28 · agent · accepted.**
- **Context:** weak roots cannot rebuild equality after the domain is torn down, and keeping the heap alive from every `Value` contradicts the teardown model.
- **Decision:**
  - A value is inline or a registered root.
  - No `PartialEq`/`Eq`/`Ord`/`Hash`; `Debug` is content-free.
  - Observation needs a live matching runtime.
- **Consequences:** clients derive host keys only through authorized observation.
- **Source:** integration plan I2A/I2B `:661-781`; 08-25 review GCI-002. **Coverage:** `architecture/evaluation.md:241-243`. The rationale is NC.

**Managed destruction is passive; every family carries a mandatory drop record** · `passive-managed-destruction-with-drop-records`
- **2026-09-02 (I4.0) and 2026-10-02 (I13A) · agent · accepted.**
- **Context:** `Trace` does not constrain `Drop`.
- **Decision:**
  - Managed destructors are passive.
  - Active cleanup lives in external-owner RAII.
  - `ManagedFamily` requires a non-empty `ManagedDropRecord` covering direct and transitive destruction.
- **Consequences:** every new family needs a reviewed destruction record.
- **Source:** integration plan `:2372-2425`; I13 cleanup `:93-108`; G4 `:36-58`. **Coverage:** `agent_context/evaluation.md:34-37`. The record is NC.

**Recursive identities are managed edges; aggregate shells stay `Arc`-shared and acyclic** · `recursive-identities-are-managed-edges`
- **2026-09-04 to 2026-09-10 · agent · accepted.**
- **Context:** cycles through lazies, promises and nets were unreclaimable.
- **Decision:**
  - Lazies, promises and nets are the only mutable recursive identities, and are exact managed edges.
  - Shells stay acyclic `Arc` until VRR.
  - Roots never stand in for internal cycle edges.
  - Promise liveness comes from producer-owned roots plus a root-free route. There are no collector weak pointers: weak pointers, failure-from-wait and stronger rooting were all rejected.
- **Consequences:** the shells become VRR input.
- **Source:** I5I10 review; integration plan `:4550-4600`, `:5202-5232`; ledger classification. **Coverage:** `architecture/evaluation.md:246-251,317-334`. The acyclicity rule is NC.

**Fresh allocations are published before regional access ends** · `publish-before-access-ends`
- **2026-09-08 · agent · accepted.**
- **Context:** fresh edges escaped regions unrooted.
- **Decision:**
  - Install or publish before access ends.
  - No self-opening constructors.
  - No root created only to bridge adjacent statements.
  - Rejected: a fresh-allocation typestate, a temp root per allocation, and recursive `Value` branding.
- **Consequences:** this is the new-family checklist (§3.3).
- **Source:** I5 review GCI5R-001/001G; integration plan `:5163-5200`. **Coverage:** `architecture/evaluation.md:304-323`. The rejections are NC.

**Owner-qualified edge-transition gateways are kept as no-op barrier sites** · `owner-qualified-edge-gateways`
- **2026-09-10 · agent · accepted.**
- **Context:** a future concurrent collector needs barrier sites.
- **Decision:** every edge-set change goes through an owner-qualified gateway. Nets report exact per-edit deltas under the net mutex.
- **Consequences:** zero cost under stop-the-world, and SATB-ready.
- **Source:** I5 review GCI5R-002; I8. **Coverage:** `SAFETY.md:1293-1337`; `architecture/evaluation.md:357-369`.

**Reflection computations trace effect and target as managed edges** · `reflection-computation-traces-effect-edges`
- **2026-09-10 · agent · accepted.** The registry half was superseded on 2026-09-19 (§4.9).
- **Context:** a reflection cycle (`meta_refl`) through registry roots.
- **Decision:**
  - The effect and gate target are direct semantic edges.
  - Rejected: parser rejection, evaluator cycle detection, and weak registry roots.
- **Consequences:** the cycle is collectible.
- **Source:** I5 review GCI5R-005. **Coverage:** `architecture/evaluation.md:571-572`.

**Persistent managed edges are move-only; raw core values are regional** · `persistent-edges-move-only`
- **2026-09-12 · agent · accepted.**
- **Context:** implicit `Copy`/`Eq` hid ownership handoffs.
- **Decision:**
  - `Gc<T>` has no `Copy`, `Clone`, `PartialEq`, `Eq`, `Debug` or `Hash`.
  - Duplication and identity are explicit, through `duplicate_in` and `same_allocation_in`.
  - `ErasedGc` is the only copyable identity.
  - Raw `core::Value` loses `Clone`/`Eq`/`Debug`.
  - Roots stay clonable without equality.
- **Consequences:** an LLVM-IR codegen latch. No moving or branding claim is made.
- **Source:** persistent-edge plan; aggressive-remediation plan D.2a. **Coverage:** `architecture/evaluation.md:336-344`; `VERIFY.md:15-21`. `SAFETY.md` is stale.

**External-only opaque payload storage** · `external-only-opaque-storage`
- **2026-09-11 · agent · accepted.**
- **Context:** a sealed managed opaque arm would need a per-type family, tracing, passive drop, and a redesign of the task-handle lifecycle.
- **Decision:**
  - Opaque payloads are passive external owners.
  - Reopen only for a concrete recursive edge whose retention is materially harmful and whose destruction can be passive, through a new design review.
- **Consequences:** a task result holding its own handle is retained until runtime teardown (an accepted cost).
- **Source:** opaque-representation review. **Coverage:** `architecture/evaluation.md:351-356` (outcome only).

**Moving collection waits for parallel-root retirement** · `moving-gc-retirement-gate`
- **2026-09-09 to 2026-09-11 · agent · accepted.**
- **Context:** parallel roots are scaffolding.
- **Decision:** moving is blocked until four conditions hold:
  1. no bare `Value` lives across a yield;
  2. no data sits beside a parallel root;
  3. `CompatibilityValueEdges` has no implementation left;
  4. every persistent edge is rewritable or stable.

  Project with `Root::as_gc`, never by caching a `Gc` beside its `Root`.
- **Consequences:** VRR prework must make progress on these conditions.
- **Source:** aggressive-remediation plan `:319-345`; I5 review GCI5R-008. **Coverage:** NC.

**Lifetime-branded `ScopedGc` is deferred until a defect demands it** · `scoped-gc-deferred`
- **2026-10-01 · agent · accepted (as a deferral).**
- **Context:** an unbranded `Gc<T>` can outlive its region; today's proof is backed by audit.
- **Decision:** defer branding. Adopt it only after reproducing a concrete defect.
- **Consequences:** no concurrent or moving readiness is claimed.
- **Source:** D2h review D2HR-004; scoped-pointer plan SP0. **Coverage:** `architecture/evaluation.md:342-344`.

**Runtimes never collect automatically (`NoAuto`); pressure is promoted only at a stable pump** · `noauto-runtime-collection-policy`
- **2026-10-02 · agent (review) · accepted.**
- **Context:** automatic collection would put a lease and wake around every outer access, and pause placement would be accidental.
- **Decision:**
  - Every heap is `NoAuto`, fixed at construction.
  - `pump_until_stable` promotes pressure to `MaintenanceRequired`.
  - Clients service it explicitly.
  - Rejected: `Automatic` by default, a hybrid, and public configuration.
- **Consequences:**
  - Ordinary entry never collects.
  - A busy runtime or the CLI may never collect (accepted; owned by Concurrent GC).
  - Any change must come as a new construction contract.
- **Source:** RuntimePolicy review; integration plan `:502-527`; 08-25 review GCI-001, GCI-015. **Coverage:** `architecture/evaluation.md:69-77,100-108`. The rationale is NC.

**Runtime-owned GC maintenance state with actionable readiness** · `runtime-owned-gc-maintenance-state`
- **2026-10-02 · agent · accepted.**
- **Context:** the anonymous `Busy` state hid maintenance.
- **Decision:**
  - One runtime record with unwind-safe leases.
  - Readiness precedence: `Busy`, then `MaintenanceFailed`, then `MaintenanceRequired`.
  - Collector statistics are never readiness authority.
- **Consequences:** any maintenance failure fails a batch.
- **Source:** readiness review `:40-170`; explicit-maintenance review. **Coverage:** partly `architecture/evaluation.md:88-95`.

**Settlement validation ignores the collector's maintenance revision** · `settlement-ignores-maintenance-revision`
- **2026-10-03 · agent · accepted.** Revisit at CG Gate 9.
- **Context:** revision churn from no-op leases rejected valid settlements.
- **Decision:**
  - Recheck work generation, exits, observation epoch, empty outputs and a clean maintenance state.
  - The revision stays only the compare-and-swap token for service.
  - Rejected: stopping no-op leases from bumping the revision.
- **Consequences:** correctness relies on non-moving, root-preserving collection.
- **Source:** regression plan D2/D3. **Coverage:** `architecture/evaluation.md:88-98`.

**Aggressive-GC verification reuses the `NoAuto` stable-pump decision** · `aggressive-gc-via-stable-pump`
- **2026-10-03/04 · maintainer (hook removal; gating NoAuto tests) and agent · accepted.**
- **Context:** a per-entry lease deadlocked against the settlement gate. The mode had been red since I12A.
- **Decision:**
  - Replace only the pressure input ("allocated since the last evaluation").
  - The pump services its own promotion.
  - Erase `Heap::enable_collection_before_outer_entry`; it is not to become an API contract.
  - Tests primarily about `NoAuto` don't run under the feature.
  - Rejected: skip on `try_read`, publication-only gate, glam-gc-driven lease, settlement as an active region.
- **Consequences:**
  - About one collection per stable cycle, not per boundary.
  - `full` takes 911 s, down from about 4 h.
- **Source:** regression plan D4–D11. **Coverage:** `architecture/evaluation.md:79-86`. D4, D9 and D11 are NC.

**Foreground GC during CLI assembly** · `foreground-gc-during-cli-assembly`
- **PENDING (holistic Decision 2).**
- **Question:** add a bounded maintenance yield to foreground demand, or keep CLI assembly non-collecting until Concurrent GC?
- **Fact:** today peak memory equals total allocation (holistic `:296-301`); NC.

### 4.8 Collector crate (`glam-gc`)

**Exact, non-moving, stop-the-world full collector; one heap per runtime** · `exact-nonmoving-stop-the-world-collector`
- **2026-08-19/21 · agent · accepted.**
- **Context:** the goal was to reclaim recursive cycles such as fixpoints, not to build a performance GC.
- **Decision:** explicit roots, no stack scan, no cross-heap edges. Moving, generational and concurrent collection need new plans.
- **Consequences:** barriers stay structural no-ops.
- **Source:** roadmap "Scope decision", invariants 1–5. **Coverage:** `SAFETY.md:122-127`; `architecture/evaluation.md:237`.

**Fixed typed-run geometry with no large-object path** · `fixed-typed-run-geometry`
- **2026-08-21 · agent · accepted.**
- **Context:** owner lookup must be constant-cost.
- **Decision:**
  - 64 KiB typed runs in 8 MiB chunks, 128 B payload alignment.
  - Owner lookup by aligned base runs.
  - Chunks are retained until heap destruction.
  - No variable runs or large objects.
- **Consequences:** objects are limited to one run, and tag-encoded run classes are deferred to VRR.
- **Source:** implementation plan C2A.1; VRR "GC-Facing Run Lookup". **Coverage:** `SAFETY.md:146-156,182-183`. The rationale is NC.

**Collection is elected at an idle outer entry; admission is a mutex and condvar state machine** · `idle-entry-election-and-condvar-admission`
- **2026-08-22 · agent · accepted.**
- **Context:** the queued-writer drain protocol was too complex.
- **Decision:**
  - Requests are coalesced hints.
  - An idle outer entry elects collection; outer exit never collects.
  - `RwLock` was rejected as non-portable in priority and unable to express dependent admission.
- **Consequences:** prepare, admit, then activate TLS entry.
- **Source:** implementation plan C3E; C2C review GC2C-003. **Coverage:** `SAFETY.md:383-463`. The rejections are NC.

**Clear-before-mark bitmaps** · `clear-before-mark-bitmaps`
- **2026-08-22 · agent · accepted.**
- **Context:** to keep the correctness surface small.
- **Decision:** clear marks before marking. No colour, journal or allocation-time mark.
- **Consequences:** mark cost includes the clearing pass.
- **Source:** implementation plan, post-C3E review. **Coverage:** mechanism at `SAFETY.md:198-200`.

**An irreversible collector invariant failure poisons the heap; it does not abort** · `poison-heap-instead-of-abort`
- **2026-08-24 · agent · accepted.**
- **Context:** a panic during sweep or finalization leaves uncertain state.
- **Decision:** permanent heap poison.
- **Consequences:** Rust resources may leak, but a destructor is never retried on uncertain state.
- **Source:** C6 review GC6-002. **Coverage:** `SAFETY.md:875-976`. The trade-off is partly covered.

**Plain mark stack; no paged range tracing** · `plain-mark-stack-no-paged-tracing`
- **2026-10-03 · agent · accepted.**
- **Context:** a 16 MiB worklist for a fan-out of 1M edges.
- **Decision:** keep `Vec<TraceWork>`.
- **Consequences:** reopen only together with a real contiguous managed container and fresh measurements.
- **Source:** C8 review C8B.3. **Coverage:** `VERIFY.md:1007-1009`. The reopen condition is NC.

**Concurrent collection stays non-moving and keeps the stop-the-world collector as oracle** · `concurrent-gc-direction`
- **2026-08-28 · agent · PROPOSED** (owned by the deferred plan).
- **Context:** idle-only election starves under overlapping heaps.
- **Decision:** concurrent marking, delayed logical sweep, and recycling gated by epoch.
- **Consequences:** open design gates 1–9 (§5.2).
- **Source:** concurrent-GC plan. **Coverage:** `architecture/evaluation.md:100-108`.

### 4.9 Superseded decisions

| Superseded decision | Date · decider | Superseded by | Date |
| --- | --- | --- | --- |
| Aggressive verification collects before every eligible outer entry (I11 hook in glam-gc) | 2026-09-11 · agent | `aggressive-gc-via-stable-pump` | 2026-10-03 |
| Settlement validation compares the maintenance revision | 2026-10-02 · agent (readiness and explicit-maintenance reviews) | `settlement-ignores-maintenance-revision` | 2026-10-03 |
| Reflection lazies keep an external stable-task-observation sidecar | 2026-09-10 · agent (GCI5R-005) | `reflection-tasks-publish-via-completion-promise` | 2026-09-19 |
| Keep exact brittle inventories through Gate G3 | 2026-10-01 · agent (D2HR-005) | `transition-negative-tests-not-inventories` | 2026-10-03 |
| New runtimes construct `Automatic` collection by default (recommended, never shipped) | 2026-08-25 · agent (GCI-015) | `noauto-runtime-collection-policy` | 2026-10-02 |
| Add a front-end no-panic fuzz target in P0 (recommended) | 2026-10-03 · agent (holistic P0-2) | `panic-discovery-by-inspection` | 2026-10-04 |

## 5. Open Work Items

### 5.1 Stale standing docs and comments (fix whatever happens to history)

| # | Location | Problem | Correct source |
| --- | --- | --- | --- |
| 1 | `agent_context/interaction_nets.md:117-122` | Describes a `CoreRuntimeNet` weak observer that I8A.0 removed. | `src/core_net.rs:133-144` (one edge, 8-byte latch); I8 review |
| 2 | `agent_context/interaction_nets.md` "Core Specialization" (~`:284-317`) | Names `CallableData`, `HostFn`, an `Error` operator and cloneable `Data`, none of which exist. Never mentions `CallableCheckpoint`. | Callable-spill plan; holistic X8 |
| 3 | `architecture/evaluation.md:571-590`, `:633`; `architecture/reflection.md:56-70`; `agent_context/evaluation.md:273-292` | Describe the removed "stable task observation" sidecar. Never mention the managed completion promise (`core.rs:1821`). | W6G1Design review; §3.5 draft |
| 4 | `architecture/evaluation.md:103-104` | Says collection runs "under exclusive mutation admission". In fact the lease is published under *shared* admission (`runtime.rs:166-174`, `gate.read()`), and the collector's own heap admission excludes mutators. | I12 review |
| 5 | `crates/glam-gc/SAFETY.md:271-274`, `:1346-1349` | Calls the `Gc<T>` standard-trait cutover "pending". | `pointer.rs:184` `…_cutover_is_closed` |
| 6 | `crates/glam-gc/SAFETY.md:1491-1496` | Defers the governing invariants to a history plan. | §3.2 draft |
| 7 | `src/README.md:82`; `architecture/reflection.md:179-185` | Describe "bounded standard-effect fusion", which no longer exists. | Holistic R4; already holistic P1-8 |
| 8 | `architecture/evaluation.md:338` | Links the persistent-edge plan, so the link will dangle. | Point it at the ADR |
| 9 | `CLI.md:39` | Says the script extension selects the compiler. | Holistic A5; pending decision |
| 10 | `agent_context/interaction_nets.md:89-90` | Says "planned checker will enforce". | Update when polarity slice 3 lands |
| 11 | Code comments | "The ownership ledger" (`core/managed.rs:184`, `value_node.rs:23`, `recursive_cells.rs:1425`) has no target once the ledger retires. Stale forward references at `reflection/protocol.rs:480` ("W8 revisits…") and `evaluation/coordinator.rs:701` ("W9C.1 profiles…"). Stale PNC `reason =` strings at `core.rs:2010-2119` and `eval/builtins/net/builder.rs:43-81` (holistic Appendix A). | §6; holistic P1-7 |
| 12 | Status lines | Holistic review: X1, X2 (aggressive fixed in `df60695d`), X4 and S3 lack notes. Parallel review: AR-003 and AR-004 unannotated. Status lines of CG, VRR, PRE and PEAF are stale. Resumable-WHNF plan header `:9-12` is stale. The G0 doc names a test that no longer exists. | §2 |
| 13 | Other holistic X8 items | `architecture/evaluation.md:389-401` (saturation); "sole mutation authority"; `LazyFailure` at `agent_context/evaluation.md:300`; module-map gaps. | Already tracked by holistic P1-8 |

### 5.2 Open items with no standing owner

Proposed home: the owning plan where one exists. Otherwise, a one-line
entry under "Open items without a plan" in `plans/README.md`.

| Item | Source | Proposed home |
| --- | --- | --- |
| Pre-GC worker stack overflow: `direct_assembly_elf --workers 4/1` exited 134. Never re-checked after the resumable-WHNF rewrite. | G0 baseline `:82-94` | ERP plan (re-check), or the panic plan's known gaps |
| Pressure tuning (survivor ratio ½, run size, class-cache width) planned "for C8" but never measured | Implementation `:3271-3283`; C8 review | VRR V0 |
| Logical tracing revisits shared persistent spines; physical dedup is future work | Roadmap `:96`; I6I7; G4 `:190`; holistic V2 | VRR V0/V-1 |
| The production-runtime Miri and sanitizer matrix is not run by any script. One target name is stale. One Miri exclusion is unresolved. | DynamicToolMatrix | `VERIFY.md` and `scripts/check.sh full` (holistic X2 residue) |
| Nightly future-compatibility warnings (deep auto-trait recursion; `fetch_update`, the latter already migrated in `4c849c19`) | DynamicToolMatrix I11D.2d | Toolchain-upgrade checklist, `AgentContext.md` "Verification" |
| Aggressive-GC rerun trigger policy | D2h D2HR-006; GCI2HR-006 | `AgentContext.md` (§7 Q5) |
| CG Gate 9 hazard (b): no allocation path may take the non-reentrant admission gate | CG plan; regression plan | `architecture/evaluation.md` "Collector Boundary" (current invariant), plus the CG plan |
| CG prerequisites N2, E3, S4 ("blocks CG0") and F5 | Holistic item 15 | CG plan CG0 |
| The CG entry criterion cites a "dated post-integration review" that does not exist | CG `:44-48` | CG plan, pointing at the G4 ADR |
| Persistent-edge follow-ups: destination-aware mutation writer; mutable edge-slot discovery before moving; revisit `Gc<T>` `Send`/`Sync` | Persistent-edge plan `:833-845` | CG Gate 2; `moving-gc-retirement-gate`; `SAFETY.md:639-647` |
| Net callable optimizations: small-set followed identity, batched pure pair steps, JIT/annotated normalization, measured specialized checkpoint | Callable-spill plan `:1407-1419` | A net performance plan (holistic N3); until then the README |
| Construction performance: budgeted or incremental large-netlist replay, batching, columnar descriptors, handlers in Glam source, split counters | Pure-construction plan `:1143-1157` | VRR (columnar) and the net performance plan |
| Indexed list-fix reevaluation is potentially quadratic (unmeasured) | PNC3 `:116-119` | Net performance plan; sibling of holistic E8 |
| Worker/client condvar split (needs a profile with workers enabled) | Resumable-WHNF plan `:9178-9184`; `spark.rs:169` | Holistic P2-9 (S1) |
| `.read.token` turns an unavailable dependency into a parser error; nested search does not suspend | Resumable-WHNF plan `:3262-3266` | README open items (macro/reflection) |
| The checkpoint-cell `try_lock` trace assumes stop-the-world | Resumable-WHNF holistic WHNFHR-005 | CG plan (already `:305-330`) and holistic E3 |
| Per-quantum edge walk and O(depth) route storage are accepted costs with reopen conditions | Resumable-WHNF holistic WHNFHR-006 `:1163-1188` | CG plan / VRR |
| A resolver given a foreign-runtime value is consumed and leaves its promise unassigned forever | I5I10 | Holistic A4 (public contract) |
| A task result that holds its own handle is retained until runtime teardown | Integration `:6047-6049`; opaque review | values.md §10 |
| CLI peak memory equals total allocation (no foreground GC) | Holistic `:296-301` | `architecture/assembly.md`; Decision 2 |
| Measurement recipe and baselines (`perf` blocked → Valgrind) | W6G4; resumable-WHNF holistic; holistic X3 | `docs/Performance.md` with P1-5 |
| Surviving negative-latch set (Decision 4). Named candidates: `gate_g2_source_inventory_is_closed`, `runtime_gc_policy_plan_has_no_live_transition`, `recursive_identity_*`. | Holistic V4 | Holistic P1-7 |
| `tests/source_doc_coupling.rs` scans `src/` only | Active-doc batch | Extend it to `tests/` and `crates/` |
| Public diagnostic projection cleanup (`Diagnostic::from_parts`) | `plans/README.md`; D2HR-003 | Stays in README open items |

### 5.3 Conflicts to reconcile

- **Disconnected subnets.** Panic plan `:575-576` says subnets disconnected
  from the public port "must never fail evaluation unless demand reaches
  it". Polarity plan `:39-45` rejects disconnected components at
  construction. Suggested reconciliation: construction rejects them, and
  only *reduction* garbage must not fail. Update the panic plan when
  polarity slice 3 lands.
- **False present-tense statements** in retirable docs. Retirement resolves
  them; until then, read them as history:
  - per-entry aggressive collection, in the integration plan I11D.1,
    I11/I11C, Closure, Remediation and G3;
  - settlement comparing revisions, in Readiness and ExplicitMaintenance;
  - the reflection sidecar, in I5 GCI5R-005.

### 5.4 Open items that stay in active docs (no action)

- **Panic plan:**
  - interaction-net inspection;
  - known gaps at `:377-386`, `:429-431`, `:471-474`, `:528-530`;
  - deep-nesting stack overflow remedies (`:54-65`);
  - diagnostic wording (`:91-99`);
  - the acceptance check (`:608-616`).
- **Polarity plan:**
  - slice 2 (in progress);
  - slice 3 (enforce at `try_finish`, invalid samples);
  - slice 4 (check after each rewrite);
  - slice 5 (the N8 generator);
  - slice 6 (fuzz);
  - deferrals at `:158-166`.
- **Holistic review:**
  - open findings X3, X5–X8, V1–V5, S1–S8, E1–E6, E8–E10, N2–N8, R1, R2, R4–R6, R8, R9, F2–F6, F8, A1, A2, A4, A5;
  - open decisions 2, 3, 5, 6, 7, and the decision 4 surviving set.
- **Parallel review:** AR-005, AR-006 (empty `.expect` still accepted at `tests/invalid_samples.rs:94,108`), AR-007, RF-001, RF-002.

## 6. Label References

### 6.1 Measurements

Lines matching each label family. "Non-inventory" means `src/**/*.rs`
excluding `*_inventory.rs`. Measured at `f3b22303`.

| Family (defining docs) | Non-inventory lines (files) | Inventory lines | Elsewhere | Example |
| --- | --- | --- | --- | --- |
| `W…`: resumable WHNF (plan, W-series) | 86 (19); 52 of them in 7 label-named test files | 178 | 15 test files named `w*.rs`; `scripts/check-interaction-net-profiling.sh:7` ("W4E") | `eval/whnf.rs:303` "Shared resumption shapes selected by the W0 census"; `reflection/protocol.rs:480` "W8 revisits whether…" (stale) |
| `I…`: GC integration phases | 40 (12) | 133 | `src/README.md:71` ("I3B"); `VERIFY.md` (1) | `runtime.rs:850` `reason = "the I6C-audited compatibility root…"` |
| `GCI…R-…`: integration review findings | 2 | 37 | `VERIFY.md` (3) | `api/value/access_inventory.rs:295` "GCI11R-002D.2e.4 rooted parser and macro orchestration" |
| `D.2…`: aggressive remediation | 9 (2) | 19 | — | `core.rs:2731` `reason = "D.2b.1b establishes the regional operation before D.2c-D.2g migrate callers"` |
| `PNC…`: pure net construction | 27 (3); 22 are `reason =` in `core.rs` | 27 | — | `core.rs:2039` `reason = "PNC2-PNC5 assemble the hidden pure builder in stages"` (stale) |
| `NC…`: callable spill | 35 (4) | 3 | 2 test files named `nc*.rs` | `eval/net.rs:3282` "NC1C budget fixture" |
| `P0–P5`: persistent edges | ~4 | — | `SAFETY.md:1349`; `VERIFY.md:20` ("P5B") | `core.rs:2893` |
| X/V/S/E/N/R/F/A and AR/RF: 2026-10-03 reviews | 0 | 6 (A3, R3, R7, E7) | none in standing docs | `eval/whnf_inventory.rs:1352` "E7 replaces the annotation machine's stderr helper…" |
| Gate `G0–G4` | 0 | 3 (`gate_g2_inventory.rs`) | `scripts/check.sh:62` and 2 glam-gc scripts ("G0 semantic regressions"); `AgentContext.md` | Collides with the language base `g0` (`every_g0_keyword_is_reserved…`) |
| `C…`, `GC6-…`: collector crate | 2 (main crate) | 4 | 36 lines in `crates/glam-gc/src`; `unsafe-modules.txt`. **All 25 distinct labels appear in `SAFETY.md`/`VERIFY.md`, so they resolve without history.** | `glam-gc/src/lib.rs:3` "C5D publishes…" |

Totals for the main crate, history-label families only (W, I, GCI, D, PNC,
NC):
- **547 lines**: 350 in inventories and 197 elsewhere, across 36 files.
- The 197 break down as 104 in test modules (84 in label-named files), 42
  lint `reason =` strings, and **51 production comment or message lines**.

Collisions observed:
- `C0` (ASCII controls) and `C3` (linearization) vs collector C0/C3;
- `g0` (language base) vs Gate G0;
- `A1`/`B1` in test data vs holistic A1.

These confirm that short IDs are poor search keys.

### 6.2 Recommendation: (b) rewrite to purpose, with (c) git history as the fallback

1. **Inventories.** These hold about 64% of the label lines. The holistic
   review's Decision 4 retires most of them, and the surviving set should
   restate each kept latch's rule in words.
2. **The 42 lint `reason =` strings.** Rewrite these in holistic P1-7
   ("`#[expect]` everywhere; resolve 'until <milestone>' comments"). Most
   are stale staging reasons, for example `"PNC2-PNC5 assemble … in stages"`.
   A reason must say why the item is unused *now*.
3. **Label-named tests.** That is 15 `w*.rs`, 2 `nc*.rs`,
   `gate_g2_source_inventory_is_closed` and
   `opaque_representation_review_inventory_is_complete`. Rename them to the
   behaviour they pin (holistic P1-7, "rename milestone tests"), and drop
   labels from fixture strings while doing so.
4. **The 51 production comment lines.** Rewrite each to state the invariant
   or the reason. For example, "Shared resumption shapes selected by the W0
   census" becomes "Shared resumption shapes: most demand sites inspect the
   result, so frames share one continuation vocabulary". Delete stale forward
   references.
5. **Fallback: git history.** The Retired history table in
   `plans/README.md` maps each label family to its defining doc and last
   commit, for example `W* → ResumableWhnfEvaluation_2026-09-12.md @
   5b8a27f3`. One `git show` then resolves any leftover label. This is a
   ~10-row family map, not a per-label glossary.
6. **Leave collector-crate labels as they are.** C-series and GC6 labels
   resolve in `crates/glam-gc/SAFETY.md` and `VERIFY.md`, which are durable.
7. **Add an agent rule.** Add to `AgentContext.md` "Working Rules": "Do not
   use plan or review labels as justification in code or tests; state the
   rule."

Options rejected:
- **(a) A glossary doc.** It keeps opaque IDs alive, needs maintenance, and
  becomes a third history artifact.
- **(c) alone.** Labels in code would still force readers to do archaeology.

## 7. Questions for the Maintainer

1. **Retention (Decision 5).** Delete with a Retired-history hash table
   (recommended), or archive in-tree?
2. **ADR home.** One `docs/Decisions.md` (recommended), or `docs/decisions/`
   with one file per decision? Should `AgentContext.md` "Where to Look" link
   it?
3. **`docs/architecture/values.md`.** Approve the outline in §3.3? It is the
   one sizable writing task, and it gates the integration plan, ledger, I13
   and readiness retirements. Alternatively, keep those 4–5 docs until VRR
   prework, which will rewrite much of that layer anyway.
4. **Ratifying agent decisions.** 37 accepted ADRs were made by agents. Mark
   them "agent, accepted by implementation", or review them individually?
   These four most deserve maintainer eyes:
   - `structural-lazy-map-and-concat` (user-visible laziness);
   - `noauto-runtime-collection-policy`;
   - `external-only-opaque-storage`;
   - `recursive-identities-are-managed-edges` (no weak pointers).
5. **Aggressive-GC cadence.** Adopt D2HR-006's change-trigger list in
   `AgentContext.md`, or keep "periodically"? Periodic runs let I12A break the
   mode for weeks unnoticed.
6. **Measurements home.** Create `docs/Performance.md` now, seeded with G0,
   C8, W6G.4, W9 and the RuntimePolicy numbers plus the Valgrind recipe? Or
   wait for the P1-5 harness and keep the two recipe reviews until then?
   Collector numbers go to `VERIFY.md` either way.
7. **`ScopedPointerSafety`.** Keep it as its own deferred plan, or merge it
   into VRR, since both say they should be sequenced together?
8. **Labels.** Approve (b)+(c) as part of P1-7, including the new
   `AgentContext.md` rule against label-as-justification?
9. **Stale standing docs (§5.1).** Fix these now as a docs-only slice ahead
   of any retirement? Items 1–6 are factual corrections, verified against
   code.
10. **Panic vs polarity on disconnected subnets (§5.3).** Confirm that
    construction rejects them and only reduction garbage must not fail.

## 8. Maintainer Answers — 2026-10-05

1. **Retention: delete.** Git is the archive. `plans/README.md` gets the
   Retired-history table (doc → last commit → label family).
2. **Decisions: one log, `docs/Decisions.md`.** A decision that needs a lot of
   explanation links out from its entry. `AgentContext.md` links the log. The
   log is marked incomplete: it starts late in development, and older
   decisions may be extracted from other sources later.
3. **`docs/architecture`.** These docs are agent-written. The maintainer
   reviews them occasionally rather than approving outlines, so `values.md`
   proceeds on the §3.3 outline at the agents' judgment.
4. **Agent decisions.**
   - The four flagged decisions involved the maintainer and are recorded as
     maintainer decisions: `structural-lazy-map-and-concat`,
     `noauto-runtime-collection-policy`, `external-only-opaque-storage`, and
     `recursive-identities-are-managed-edges`.
   - The other agent decisions are accepted.
   - `no-semantic-recursion-on-rust-stack` stands, but it does not yet cover
     parsing `.g` files: the parser still recurses on the Rust stack. The
     deep-nesting remedy belongs to the panic plan.
   - Pending ADRs move into the plans that should decide them, not the log.
5. **Aggressive-GC cadence: change triggers.** Adopt the change-trigger list
   in `AgentContext.md` in place of "periodically".
6. **Measurements: wait for the P1-5 harness.** Keep the two recipe reviews
   (resumable-WHNF holistic and W6G.4) until then. Collector numbers still
   go to `VERIFY.md`.
7. **`ScopedPointerSafety`: keep it separate.**
8. **Step IDs in code.** This report's "labels" are better called *step IDs*,
   since glam uses "label" for a dictionary key.
   - Step IDs are fine temporarily, while their work is active.
   - Cleanup phases such as this one replace each with a `Decisions.md`
     reference or a short explanation, whichever is more convenient.
   - Adopt (b), with (c) git history as the fallback, and add the rule to
     `AgentContext.md` "Working Rules" (done).
   - Wave 6 below applies it to production comments, lint `reason` strings,
     and the names of tests and test files. Inventories are handled when
     they retire.

## 9. Execution Waves (updated after §8)

Progress:
- Waves 1–3 landed in `8b1ace22`: standing-doc corrections, `Decisions.md`,
  and `values.md`. The §3 extractions are still pending.
- Wave 4: `plans/README.md` is now the index.
- Wave 5, first batch: the 27 RETIRE docs are deleted and listed in the
  README's Retired History.

1. **Outdated standing docs** (§5.1 items 1–8, 12 and 13), plus the panic
   plan's disconnected-subnet wording. Items 9, 10 and 11 wait on Decision
   7, polarity slice 3, and wave 6 respectively.
2. **`docs/Decisions.md`** from §4, with the §8 answers applied. Pending
   drafts move into their owning plans. `AgentContext.md` links the log and
   states the change-triggered `full` rule (done).
3. **`docs/architecture/values.md`** from §3.3. Then apply the §3.2–§3.6
   extractions to the standing docs.
4. **`plans/README.md` becomes the index:**
   - active plans and reviews;
   - deferred plans;
   - open items without a plan (from §5.2);
   - retired history, mapping each doc to its last commit and step-ID family.
5. **Delete retired docs** in the §1.4 order: the RETIRE set first, then each
   EXTRACT doc once its extraction lands. The two measurement-recipe reviews
   stay until the P1-5 harness.
6. **Replace step IDs in code** with `Decisions.md` references or short
   explanations. This covers about 51 production comment lines and 42 lint
   `reason` strings, and renames the label-named tests and test files.
   Inventory step IDs go with the inventory retirement (holistic Decision 4).

**Found during execution** (fix in the wave noted):
- `drain_retired`'s doc comment says owners after a panicking destructor wait
  for the next drain. The code catches the panic and keeps draining, and a
  test confirms it. (Wave 6, code.)
- `architecture/evaluation.md` calls the publication nursery a production
  handoff shape, but only test constructors use it. (Wave 3.)
- Production drains external owners only in `HostCallProducer::invoke` or at
  teardown; `values.md` now says so. (Done.)

**Follow-up cleanup** (maintainer, 2026-10-05; not part of this plan): review
comments for ones that are too large and belong in a doc, or that are
mostly redundant because the code already says it.
9. **Stale standing docs: fix now,** as a docs-only slice (wave 1).
10. **Disconnected subnets:** update the panic plan to match the polarity
    rule. Construction rejects disconnected components; only reduction
    garbage must not fail.
