# Resumable WHNF W7 Review — 2026-09-27

Baseline: `da14956f`, after W7C completion.

Status: complete. W7 closes the intended production Rust-stack, small-stack,
budget, owner-yield, and fairness boundaries. This review found no production
correctness defect. It repaired two verification gaps, accounted for one
intentional inventory change within W7, and tightened W8's migration and
verification instructions.

## Scope and method

This review audits W7A-W7C against the current implementation and checks W8-W9
for drift from the as-built boundary. It inspected:

- the W0B/W7 source census and local call-edge ledger;
- the iterative dictionary path, reflection API path, and key-conversion
  implementations introduced by W7;
- the W7 small-stack and scheduler-owner fixtures;
- exact evaluation budgets, foreground reservation, runtime background
  pumping, and client/reflection/spark release paths;
- current evaluation architecture and invariant documents;
- the exact six-declaration W8 compatibility handoff; and
- W4E's profiling-only net-work-item fuse in relation to W7's production
  semantic-transition budget.

The review ran all W7-named tests under ordinary and aggressive-GC builds and
the complete 119-test inventory suite before remediation. Focused remediations
were then run independently before the closing repository gates.

## Outcome

The implemented W7 boundary is coherent:

- the production census contains no unapproved direct recursion, and the
  locally resolvable production call graph contains no cycle;
- the only direct-evaluator exception remains the exact six-declaration W8
  family, which ordinary resumable owners do not enter;
- user-sized evaluator traversal uses explicit work state or an approved
  balanced persistent-container bound;
- one shared stack-owned `EvaluationStepBudget` limits a claimed machine poll,
  while foreground exact-demand pumping deliberately uses a conservative
  reservation allowance;
- ordinary yields restore the same coordinator work record without publishing
  a dependency;
- client and reflection queues requeue FIFO within their role, while worker
  task/spark selection alternates across roles; and
- worker, runtime-background, and foreground selectors retain their distinct
  authority boundaries.

The review does not reinterpret source-inventory counts as runtime traffic, or
routine family-depth fixtures as a proof that every historical recursive
implementation would overflow exactly the selected stack. Stack closure comes
from the source gates, explicit state representations, common resumable
driver, and representative bounded executions together.

## Findings and resolutions

### WHNFW7R-001 — Resolved: client and spark budget-yield tests bypassed their real poll paths

**Severity:** medium verification gap; no observed production defect.

The client FIFO fixture claimed a ready client and directly released
`ClientDemandPoll::Yielded`; its input was an immediate number, so no client
machine had actually exhausted a budget. The spark fixture likewise released
`SparkWorkPoll::Yielded` directly. Those tests proved coordinator requeue
transitions, but not the W7C requirement that the owner-specific poll adapter
translate a real WHNF budget yield into those transitions.

An attempted direct substitution with an outer lazy usefully demonstrated the
ownership distinction: the real client and spark polls became `Blocked` on the
separately owned lazy route, exactly as designed. The repaired fixtures use
already-assigned promise spines directly. They now force the client and spark
machines themselves to consume a complete quantum, publish no dependency, and
requeue the same work records. The client fixture retains an explicit
`A, B, A, B` claim/poll trace; the spark fixture reclaims the same stable work
ID. Direct release remains only for terminal test cleanup after the behavior
under test has completed.

### WHNFW7R-002 — Resolved: the recursive control accepted any abnormal child exit

**Severity:** low test-oracle gap.

The W7B control subprocess previously proved only that its exit status was
unsuccessful. An unrelated panic, abort, or harness failure could therefore
have satisfied the negative control. The child now captures stderr and must
report `stack overflow` in addition to exiting unsuccessfully. The semantic
worklist fixtures continue to complete on the same requested 512 KiB stack.

### WHNFW7R-003 — Confirmed: one initial checkpoint root is intentional

**Severity:** none; accounting clarification.

The no-churn requirement applies after the aggregate checkpoint is installed.
The first retained WHNF transition registers one steady checkpoint root; later
polls add neither roots nor managed slots. Attempting to strengthen W7C into a
zero-root installation rule failed deterministically and conflicted with the
reviewed W6G ownership design. The existing root baseline is therefore kept:
one durable checkpoint owner, not one root per continuation frame or repoll.

### WHNFW7R-004 — Accepted: W7B intentionally advances the W7A inventory baseline

**Severity:** none; justified chronological drift.

W7A closed at 158 occurrences, including 90 explicit iterations, 49
orchestration occurrences, and 19 W8 compatibility occurrences, with 1,148
locally resolved calls. W7B then replaced typed-receiver recursion in key
conversion with one explicit parent stack. Its iterative trace visitor added
one reviewed loop and the refactoring added six locally resolvable call edges.

The current exact baseline is therefore 159 occurrences: 91 explicit
iterations, 49 orchestration occurrences, and the same 19 W8 compatibility
occurrences. The local call graph contains 1,154 edges and no cycle. W7A's
completion record remains a chronological checkpoint rather than being
rewritten to pretend the later W7B representation already existed. Current
source constants and tests latch the final values.

### WHNFW7R-005 — Accepted: routine producer-family depths compose with the structural proof

**Severity:** none; deliberate verification cost boundary.

The common explicit WHNF worklist and promise-alias witness run at depth 4,096.
Producer-heavy lazy, application, and fixpoint chains use depth 512, while the
owner-handoff matrix uses depth 128. Those smaller values keep routine tests
from turning linear scheduler setup into a large suite tax. They still cross
many semantic transitions and force suspension onto a distinct named poller.

This would be inadequate as the only stack-safety evidence. Here it composes
with the exact production recursion census, cycle ledger, compile-exhaustive
owner representations, 4,096-level common-driver/control witness, and deep
structural conversion fixtures. No additional ignored scale test is justified
without a distinct implementation path or observed regression.

### WHNFW7R-006 — Resolved by plan: W8 still treated W7 as a possible replacement for the W4E fuse

**Severity:** low forward-plan precision.

W7 budgets semantic evaluator transitions. W4E's profiling-only fuse counts
every interaction-net driver work item and can stop in the middle of a
normalization batch before another semantic transition exists. They are
deliberately different boundaries. W7 therefore cannot replace the W4E fuse
while preserving that inverse fixture's exact 16-work-item boundary.

W8 now explicitly retains the static profiling fuse unless a separately
reviewed net-work budget is introduced. W8D also names the focused interaction-
net profiling script, W7 ordinary/aggressive fixtures, and exact inventories
as closure gates instead of relying on the broad suite to imply them.

### WHNFW7R-007 — Resolved by plan: W8B.1 is too broad until its caller census partitions it

**Severity:** low checkpoint-size risk.

The six compatibility declarations are still exact, but `eval_value` is a
widely used test convenience surface. W8A.0 already requires a caller census;
the plan now requires that census to partition test-helper migration into
bounded batches before W8B.1 begins. Production compatibility removal and
mechanical test migration must not become one unreviewable checkpoint, and a
new test helper must drive the shared resumable path rather than reconstruct
the direct evaluator under another name.

## Future-phase accounting

W8 remains the correct next phase. Its entry latch is still six
`W8ValueCompatibility` declarations at fingerprint
`1_485_448_540_290_006_633`, with 19 W0B occurrences at fingerprint
`12_624_383_794_632_597_732`. No ordinary production owner enters that family.
W8 should decide `EvaluationHalt` from the live caller census, delete rather
than rename the exception, and then rerun W7's stack and budget closure.

W9 remains correctly isolated. W7 changed no exact-route generation or
release-accounting semantics. W8 may change call shapes and measured traffic,
so W9 must still reproduce or replace its Callgrind baseline after W8 rather
than treating the old counts as current.

The public resumable-evaluation and pure-effect-fusion plans remain independent
follow-ups. W7 establishes private scheduler and budget contracts they may
later expose or optimize; it does not pull either API into W8.

## Verification

Review baseline:

```text
cargo test -q --lib w7
    22 passed
cargo test -q --features aggressive-gc-verification --lib w7
    22 passed
cargo test -q --lib inventory
    119 passed
```

Closing verification:

```text
cargo fmt --check
    passed
cargo clippy --all-targets --all-features -- -D warnings
    passed
cargo test -q --lib w7
    22 passed
cargo test -q --features aggressive-gc-verification --lib w7
    22 passed
cargo test -q --lib inventory
    119 passed
cargo test -q
    1,861 library tests passed; 2 ignored; every integration group passed
scripts/check-interaction-net-profiling.sh
    every named profiling fixture passed
```

The focused results include the repaired W7C suite and stack-overflow control.
No uncontrolled repeated schedule is used as concurrency evidence.
