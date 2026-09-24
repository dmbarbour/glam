# Resumable WHNF W6G Review — 2026-09-24

Baseline: `05b7d368`, after W6G.5a integrated verification.

Status: complete. W6G's ownership, scheduler, checkpoint, and measured
performance boundaries are coherent. This review found one present-tense
documentation error and two forward-plan precision problems, but no new
production correctness defect. The documentation error is fixed here; W7 and
W8 are repartitioned around exact inventories and decision gates. W6G4R-003
remains owned exclusively by Phase W9.

## Scope and method

This is the mandatory W6G.5b review from the
[resumable-WHNF plan](../plans/ResumableWhnfEvaluation_2026-09-12.md). It
audits completed W6G.1, extracted W6G.2, completed W6G.3, the W6G.4 measured
repair, and W6G.5a's integrated evidence. It also reviews W7-W9 against the
as-built implementation for drift and checkpoint size.

The audit inspected the current coordinator selectors, lazy-route admission
and release, exact-demand zipper, managed lazy checkpoint ownership, WHNF and
raw-value inventories, reflection request probes, effect-machine driver, and
current architecture/invariant documents. The source-backed inventory suite
passes all 116 tests at this baseline. W6G.5a already ran the forced scheduler,
ordinary/aggressive-GC checkpoint, profiling, Clippy, formatting, and complete
workspace matrices; this documentation review does not repeat those full
Rust gates.

Static occurrence counts below are deliberately called **source inventories**,
not runtime traffic. Dynamic traffic claims come only from probes or profiles
which count an executed fixture.

## Outcome

The final W6G shape matches the intended ownership split:

- foreground clients own private demand handles and non-authoritative exact
  routes; workers never claim foreground roots;
- workers start from reflection roots or sparks, the runtime background pump
  starts from reflection roots, and both follow only exact causal producers;
- one managed lazy owns its source, partial checkpoint, and terminal cache;
  a coordinator lazy route contains only admission, dependency, and demand
  accounting—not duplicated semantic progress;
- activated reflection work remains an autonomous coordinator machine and
  publishes into a managed completion promise;
- one managed WHNF cell owns the complete focus and continuation state, with
  one aggregate edge transition per bounded poll;
- the bounded W5 standard-effect driver remains the production reference;
  regional pure-effect fusion is explicitly deferred; and
- demand sessions are lifecycle/reporting groups rather than execution lanes.
  Independent same-session records may be claimed concurrently.

No inspected path restores a coordinator-owned WHNF continuation, a
registered root inside the managed value graph, worker authority over a
foreground root, or a session-wide one-machine admission scan.

## Boundary accounting

| Boundary | Current evidence | Judgment |
|---|---|---|
| Scheduler traffic | The W6G.4 closing fixture records 19,499 retained-route handoffs and 9,356 cold fallbacks. Callgrind closes at 3,339,481,894 instructions with an unchanged interaction-net signature and structured diagnostic. | Dynamically measured. The remaining broad `Contention` class is the narrowly defined W6G4R-003 residual owned by W9. |
| Managed-access surface | The syntax-backed admission ledger contains 284 exact entries, 40 production entries, no production recursive introduction, and no production lexically nested introduction. Focused W6G fixtures additionally assert depth one at structured construction/poll boundaries. | Source shape plus focused dynamic assertions. This is not an end-to-end count of access entries in the assembly fixture. |
| Root publication | The root-publication ledger contains 206 exact sites, 54 production sites. W6G.3's one- and 32-frame fixtures retain one steady WHNF root, publish no root per repoll or retained frame, and return to baseline after release/collection. | Source shape plus exact checkpoint-fixture counts. No root-per-semantic-step regression is present. |
| Checkpoint construction/conversion | The durable-WHNF inventory contains 73 exact call sites: 17 ordinary seeds, one application checkpoint, two promise roots, two scalar observations, and 51 source-owner modifiers. The former root-per-field projection/reconstruction representation is absent; managed polls mutate one canonical cell. | Exact source boundary and deterministic conversion/root probes. The count is construction API shape, not poll frequency. |
| Reflection request dispatch | The forced replay fixture constructs, parses, and dispatches each of its three authored requests exactly once after suspension; branch/failure fixtures retain their authored counts. | Exact executed counts for the replay-sensitive boundary. A whole-program dispatch profile is not needed for W6G correctness. |
| Standard effects | The bounded W5 driver remains the semantic and performance reference and shares the caller's mutable step budget. W6G introduced no second effect interpreter. | Intentional deferral. Post-representation access/root/dispatch measurement belongs to the [Pure Effect Access Fusion plan](../plans/PureEffectAccessFusion_2026-09-23.md). |
| Same-session admission | `ready_selection_allows_independent_same_session_machine_claims`, the running/retired deferred admission fixtures, and the two-worker fixture force independent same-session progress. The old running-machine scan is absent. | Final rule implemented and documented: session membership grants neither serialization nor unrelated helping authority. |

The static ledgers are valuable closure guards, but they cannot answer how
often a source site executes. W7C owns exact budget/poll/root measurements for
deep resumable evaluation. PEAF1 owns later whole-chain access, root,
internal-copy, and request-dispatch measurement after value representation is
no longer temporary. This is explicit ownership of the residual measurement,
not unfinished W6G work.

## Ownership and semantic audit

### Foreground and background roles

`select_worker` begins at reflection or spark roots;
`select_runtime_pump` begins at reflection roots and excludes sparks;
session drain begins at reflection work owned by that session; and foreground
driving begins at its exact client record. Exact dependency and causal-child
helping remain separate: `.task.new` launch provenance can be helped, but is
not an exact zipper edge or implicit join. These distinctions are represented
in selectors rather than metadata attached to a generic machine.

The exact route is appropriately non-authoritative. Its frames contain work
identities, subscription epochs, and dependency keys only. Claim and
validation occur under coordinator state, and any insufficient proof falls
back to the complete guarded traversal. It owns neither managed values nor a
demand-session lease.

### Lazy and reflection progress

Production `LazyTaskWork` variants are all unit route markers. The transient
`LazyTaskMachine` is reconstructed for one claimed route poll and obtains
semantic state from the managed lazy. `LazyRouteWork` retains the lazy root,
wait, published dependency block, and demand accounting required by the
coordinator; it is runtime-owned and session-neutral. This is the final route
adapter shape, not the pre-W6G state-bearing producer machine.

Reflection correctly remains different. Once activated, a reflection task
runs to a terminal disposition as autonomous work and fulfills its managed
promise. Route loss can retire the observer's lazy route without cancelling
the reflection machine. Failure acknowledgement is carried by the promise's
edge-free producer/reporting authority rather than a task handle embedded in
the managed graph.

### Checkpoint tracing and publication

`ManagedLazyCheckpointCell` owns one canonical `WhnfState`. Its trace and
mutation gateway use the same exhaustive edge visitor; one bounded poll sees
one leaving state and one adding state. Poison recovery is tracing-only, while
ordinary repoll reports the poisoned evaluator state. Dependency admission,
callbacks, reflection activation, scheduler release, and host work occur only
after the cell lock and managed-access region close.

The current mutex-backed trace contract is valid for the stop-the-world
collector. Concurrent marking and a likely trace-immediate `RootFrame` remain
owned by the concurrent-GC plan and are not silently assumed here.

## Findings and resolutions

### WHNFW6GR-001 — Resolved: architecture still predicted removal of the final lazy route adapter

**Severity:** low documentation drift.

`docs/architecture/evaluation.md` described the final state-bearing progress
boundary accurately, then said W6G.1f.2b would remove the coordinator route
machine. W6G.1f.2b instead removed semantic state and observer-session
ownership from that record; the runtime-owned, state-free route adapter is
still required for admission, exact dependency publication, claims, and
last-demand retirement.

**Resolution:** the current architecture now describes that final route
record and its retirement policy without a future W6G reference. W8C will
still perform the broader final documentation audit after compatibility
deletion.

### WHNFW6GR-002 — Resolved by plan: W7A could not prove its stated closure from W0B

**Severity:** medium forward-plan precision.

W0B detects direct self-calls, selected evaluator operations, coordination
boundaries, and source loops. Its 207 occurrences are classified by
resumption shape, not by the W7 acceptance categories of bounded structural
recursion, budgeted semantic work, scheduler orchestration, separately owned
worklists, and forbidden user-controlled Rust recursion. It also does not by
itself prove absence of mutual recursion.

Three remaining evaluator signals live in the exact six-declaration W8
compatibility family. Requiring W7 to reduce the complete manifest to zero
before W8 removes that family would make the phase order self-contradictory.

**Resolution:** W7A is split into a disposition/call-graph gate, production
stack closure, and an exact W8 handoff. W7 must reach zero unapproved
production semantic recursion while retaining only the fingerprinted six-item
W8 exception. W8 must remove that exception and rerun closure before the whole
plan completes.

### WHNFW6GR-003 — Resolved by plan: W7 combined three distinct verification decisions

**Severity:** medium checkpoint-size and contract ambiguity.

The old W7B combined the small-stack harness with seven semantic families and
both uninterrupted/suspended forms. The old W7C combined the meaning of
budget, owner-specific requeue behavior, fairness, and root/allocation
measurement. In particular, `EvaluationStepBudget::spent()` counts actual
inner transitions, while the foreground pump currently reserves a complete
task quantum before polling and does not refund unused allowance. “Exact
budget” was therefore ambiguous.

**Resolution:** W7B now separates the harness, alias/application/fixpoint
chains, structural paths/collections/failures, and scheduler-owner matrix.
W7C begins with an explicit spent-step versus admitted-quantum decision gate,
then separately verifies checkpoint exactness, owner requeue semantics,
forced fairness, and root/allocation accounting. No concurrency claim may be
closed by repetition.

### WHNFW6GR-004 — Resolved by plan: W8 named behavior but not the exact compatibility surface

**Severity:** low forward-plan drift.

The live D.2c manifest is now only six `ValueDemand` declarations with one
exact fingerprint, but W8A/B described overlapping removals without making
that set their entry/exit latch. This could let compatibility wrappers move
between the two checkpoints or leave the direct evaluator constructor alive
after its recursive wait transport disappeared.

**Resolution:** W8 now begins from the exact six-item manifest, separates
retryable transport deletion from direct-wrapper/call-site deletion, makes the
`EvaluationHalt` disposition a post-caller-census decision, and requires the
W8 compatibility manifest to reach zero. W8D repeats the relevant small-stack
and budget fixtures after deletion.

### WHNFW6GR-005 — Accepted residual: broad dynamic access/root/request profiling is deferred deliberately

**Severity:** none for correctness; future performance evidence.

W6G dynamically measured scheduler route traffic because profiling identified
it as the dominant regression. It used exact isolated probes and source
inventories for managed access, root publication, checkpoint conversion, and
request dispatch. Adding permanent counters to every boundary merely to
produce one aggregate W6G number would enlarge hot paths without a repair
decision.

**Disposition:** W7C measures these costs where they are stack/budget
invariants. PEAF1 performs the broader standard-effect-chain measurement after
Value Representation Refinement, when root and value-copy costs are stable
enough to guide an optimization. No missing profiler is left implicitly owned
by W6G.

### WHNFW6GR-006 — Confirmed: W6G4R-003 remains isolated in W9

**Severity:** none.

W7 owns semantic stack and budget behavior; W8 owns compatibility retirement.
Neither should tune exact-route generation accounting. W9 already requires a
fresh baseline after W7-W8, separates classification from repair, forces every
relevant claim/poll/release ordering, retains guarded cold traversal as the
authority, and keeps or reverts the optimization based on deterministic
instruction work. No W6G.5 task duplicates or broadens it.

## Forward execution order

1. W7A upgrades the census and closes production stack ownership while
   carrying the exact W8-only compatibility exception.
2. W7B establishes small-stack behavior by bounded family checkpoints.
3. W7C selects one budget vocabulary, proves exact resumption/requeue/fairness,
   and records root/allocation/poll diagnostics.
4. W8A-B delete the six compatibility declarations and the direct evaluator
   gate; W8C-D close inventories, current documentation, and full verification.
5. Rebase W9's classification baseline on the resulting coordinator, then
   investigate W6G4R-003 without altering semantic scheduler ownership.

The deferred [public resumable evaluation](../plans/PublicResumableEvaluation_2026-09-23.md)
and pure-effect fusion plans remain independent follow-ups. W7 may improve the
private budget contract they will later expose or optimize, but it does not
pull either public API or regional effect representation into scope.

## Verification performed by this review

```text
cargo test -q --lib inventory
    116 passed; 0 failed
git diff --check
```

The final `git diff --check` result is recorded when the review and plan edits
are complete. No Rust source changed, so the full Rust routine suite is not
repeated after W6G.5a's complete pass.
