# Resumable WHNF W8 Review — 2026-09-27

Baseline: `98964f42`, after W8C completion.

Status: complete. W8 retires the direct evaluator gate and whole-value
compatibility wrappers, closes the D.2c raw evaluator partition, and reconciles
the current architecture with the implemented resumable boundary. This review
found no production W8 correctness defect. It records one test-fixture managed
ownership gap and the expected repository-wide aggressive-verification gaps
owned by later GC remediation phases; Gate G3 remains closed.

## Scope and method

This review audits W8A-W8C against the implementation and the plan's twenty
semantic/safety invariants, verification matrix, and completion criteria. It
also checks forward drift in D.2d-D.2g, persistent-edge P3-P5, Gate G3, and
W9. The review inspected:

- the shared synchronous client-demand driver and `EvaluationHalt` boundary;
- the exact source ledger for all retired direct evaluator names;
- the closed D.2c raw-value manifest;
- `EvaluatorStepContext` admission and bounded test-only regional entry;
- WHNF seed promotion, managed checkpoint ownership, and hosted reflection
  request decoding;
- every managed specialized lazy-checkpoint family named by the ownership
  ledger; and
- the remaining orchestration, front-end, reflection, and public API
  raw-value partitions which still block Gate G3.

No W8 change adds unsafe code or synchronization, so Miri and model checking
are not implicated. Existing forced-ordering probes, barriers, and small-stack
controls remain the concurrency evidence; repeated runs are not used as proof.

## Outcome

The implemented W8 boundary is coherent:

- whole-value synchronous callers enter runtime-owned client demand;
- regional tests have one explicitly test-only bounded normal-poll entry;
- `EvaluatorStepContext` derives access only from its admitted poll carrier;
- the direct compatibility admission and `eval_value`, `eval_value_in`, and
  `eval_lazy` wrappers are absent and guarded by an exact source ledger;
- D.2c has one fail-closed empty-partition assertion rather than historical
  family, checkpoint, and fingerprint machinery;
- client, promise, reflection, spark, and lazy owners retain the same canonical
  WHNF state across yield and dependency boundaries; and
- current architecture and ownership documents no longer describe the deleted
  gate as live.

The complete ordinary workspace suite passes. Focused W7, D.2c, inventory,
and interaction-net profiling verification also passes, including aggressive
collection for W7 and D.2c. The complete aggressive workspace suite still
fails in the later raw-value ownership partitions and then fails to settle
after reaching the end of the main test list. This was already the declared
I11D.1/Gate G3 state; W8 neither hides nor widens it.

## Invariant accounting

| Invariant | Review result |
| --- | --- |
| 1-2: exact continuation and same semantic identities after suspension | Satisfied. `WhnfComputation` retains one canonical managed state; W0A/W4/W5 forced probes and W7 resumptions remain green after compatibility deletion. |
| 3: dependencies are not permanent failures | Satisfied. Poll outcomes retain distinct dependency, yield, external-boundary, and rooted-failure cases; W8 removed only the recursive compatibility projection. |
| 4: delegation has no semantic side effect | Satisfied. The regional driver replaces focus/state without constructing a lazy, promise, wait, task, or cache record. |
| 5-6: immutable lazy recipe and destination-owned publication | Satisfied. Lazy progress is traced beneath its owning lazy; client/reflection/promise/spark machines own their separate destinations. |
| 7-8: no regional value escapes; every durable value has an exact owner | Satisfied for production D.2c. The ownership ledger names `ManagedWhnfRoot` and every specialized checkpoint cell. A test-only counterexample is recorded below and belongs to the later repository closure. |
| 9: uninterrupted delegation adds no durable-owner traffic | Satisfied by the aggregate checkpoint/root-registration probes retained from W6G/W7. |
| 10: no wait, callback, coordinator action, poll, or sleep under access | Satisfied. The direct gate is gone; hosted request decoding roots before dispatch, and external boundaries return to the outer machine after access closes. |
| 11-14: canonical work sharing, cache/promise distinctions, cycle policy, and once-only reflection | Satisfied by the unchanged managed lazy/promise cells, coordinator dependency graph, and forced reflection activation probes. W8's fixture migration uses those production owners rather than reproducing them. |
| 15: structured failure-context order | Satisfied in the ordinary suite and focused suspension fixtures; the API aggressive failure below occurs before semantic evaluation and does not contradict ordering semantics. |
| 16-17: raw nets are WHNF and cursor contention is the sole structural wait exception | Satisfied. W8 changes neither net semantics nor the bracketed cursor wait. |
| 18: user-sized WHNF recursion is iterative or explicitly bounded | Satisfied by the W7 source census and small-stack controls, rerun after wrapper deletion. |
| 19: deterministic budget yield | Satisfied. Nested WHNF/list-front work borrows one mutable budget; W7 ordinary and aggressive budget fixtures remain green. |
| 20: ordering claims use controlled schedules | Satisfied. W8 introduces no new race claim; inherited claims retain barriers, channels, probes, or model evidence. |

## Findings and resolutions

### WHNFW8R-001 — Resolved: current documentation and the raw manifest retained deleted compatibility concepts

**Severity:** medium documentation and closure-gate drift.

The implementation had deleted the direct gate, but the current evaluation
architecture, agent invariants, source map, and ownership ledger still
described it as live. The D.2c raw inventory also retained empty historical
family/checkpoint/fingerprint machinery after its partition reached zero.

W8C now documents the canonical WHNF submachine and hosted request decoder,
names every durable checkpoint owner, and uses one exact empty D.2c assertion.
Chronological detail remains in plans and reviews rather than current
architecture documents.

### WHNFW8R-002 — Accepted future remediation: repository-wide aggressive verification still fails outside D.2c

**Severity:** high Gate G3 blocker; not a W8 production regression.

The full aggressive command deterministically exposed failures in public API,
evaluation lifecycle, macro/compiler, reflection-machine, and reflection-store
fixtures. Representative failures report unallocated or foreign managed edges
while tracing public value nodes, list-effect checkpoints, or later-phase
owners. The run then stopped settling after reaching the final portion of the
main test list and was terminated after nearly nine minutes.

Focused `--features aggressive-gc-verification --lib w7` and `--lib d2c`
both pass. The raw manifest also reports no production D.2c occurrence.
Accordingly these failures remain assigned to D.2d-D.2g and I11D.1 rather than
being masked by a W8 compatibility path. Gate G3 remains closed, and production
collection remains `NoAuto`.

### WHNFW8R-003 — Accepted future remediation: one net contention fixture carries an unrooted test facade between access regions

**Severity:** medium verification-fixture ownership gap; no observed production
defect.

`two_workers_contend_for_one_linear_checkpoint_payload` constructs a
`CoreRuntimeNet`, returns its raw facade from one managed region, then opens a
new test access to inspect it. Aggressive collection correctly rejects that
unrooted handoff before the contention schedule begins with “managed pointer
does not belong to this heap.” The ordinary forced barrier schedule passes.

The repair belongs to the later raw orchestration/test-fixture closure: retain
an exact net root across the handoff or keep construction and initial
observation inside one bounded access. Do not weaken heap validation or add a
durable claim to the net. This fixture does not invalidate W8's WHNF ownership
or concurrency semantics because it fails before installing the checkpoint.

### WHNFW8R-004 — Accepted interlock: persistent-edge P3-P5 and Gate G3 do not close with D.2c

**Severity:** none; forward-plan accounting.

D.2c reaching zero removes the evaluator/builtin consumers, but the exact 51
carrier dependencies remain owned by D.2b and D.2d-D.2g. P4/P5 therefore remain
closed until that manifest reaches zero. W8 introduces no replacement implicit
edge trait and provides no authorization for automatic collection.

### WHNFW8R-005 — Confirmed future work: W9 remains correctly isolated

**Severity:** none; forward-plan accounting.

W8 changes evaluator admission and fixtures, not exact-route claim/release
generation semantics. W9's classification-first repair therefore remains the
right next phase, but it must reproduce or replace its measured W6G4 baseline
after W8 rather than assuming the old traffic is current. The conservative
guarded fallback remains authoritative meanwhile.

## Completion-criteria accounting

Criteria 1-10 are satisfied for the W0-W8 semantic transition: exact durable
resumption, no reflection replay, iterative budgeted WHNF, bounded access,
root-free uninterrupted delegation, owner-specific publication, classified
retryable callers, deterministic focused verification, bounded source-shaped
work, and this post-implementation review are all present. Criterion 8 is not
misread as repository-wide Gate G3 certification; W8's applicable collection
matrix passes while the separately inventoried D.2d-D.2g gaps remain open.

Criterion 11 is deliberately owned by W9 and keeps the overall resumable-WHNF
plan open. W8 is complete; the larger GC integration is not.

## Verification

```text
cargo fmt --check
    passed
cargo clippy --all-targets --all-features -- -D warnings
    passed
cargo test -q --lib w7
    21 passed
cargo test -q --features aggressive-gc-verification --lib w7
    21 passed
cargo test -q --lib d2c
    1 passed
cargo test -q --features aggressive-gc-verification --lib d2c
    1 passed
cargo test -q --lib inventory
    116 passed
cargo test -q
    1,858 library tests passed; 2 ignored; every integration group passed
scripts/check-interaction-net-profiling.sh
    every named profiling fixture passed
cargo test -q --features aggressive-gc-verification
    failed in later raw-owner partitions and did not settle; Gate G3 remains closed
```

No uncontrolled repeated schedule is used as concurrency evidence.
