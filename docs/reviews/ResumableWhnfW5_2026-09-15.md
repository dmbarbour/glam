# Resumable WHNF Phase W5 Review — 2026-09-15

Baseline: W4E closure at `be4bedf`; W5 implementation and verification end at
`d156be6`, followed by the documentation and paired-oracle corrections in this
review.

Status: review complete. W5 closes reflection request decoding and every
production specialization callback over the resumable WHNF protocol. No open
W5 correctness defect or semantic question was found. W6 is gated on an exact
inventory reconciliation and checkpoint partition rather than beginning from
its current family-sized headings.

## Scope

This is the mandatory post-W5 review required by
[`ResumableWhnfEvaluation_2026-09-12.md`](../plans/ResumableWhnfEvaluation_2026-09-12.md).
It audits:

- ownership of effect-object selection, application results, decoded requests,
  specialization work, scalar demands, paths, and control state;
- suspension, dependency resumption, transaction-conflict reconstruction,
  branch fallback, cancellation, and terminal failure;
- exactly-once host observation and callback behavior;
- root and task-record lifetime across request waits;
- deterministic verification of replay, structured failure context, and
  abandoned alternatives; and
- drift in W6-W8 and the parent raw-value migration plan.

## Outcome

`EffectTask` now owns non-cloneable work for each suspendable phase. Effect
decoding advances through the effect object, selected function, application
result, and durable request decoder. A decoded specialization request moves
into the specialization's own `RequestWork`; scalar, state-path, and control
requests likewise retain explicit work rather than re-entering a recursive
evaluator call.

Every production `TaskSpecialization` constructs this owned request work
directly. `RequestContext` contains host, transaction, value-access, and
isolated-search capabilities, but no general evaluator service. The former
synchronous adapters and reflection-local recursive evaluation helpers are
absent. One already-WHNF application compatibility helper remains in the
effect machine and the bounded `TaskHalt`/`EvaluationHalt` conversion remains
in the protocol; both have explicit W6/W8 owners.

## Semantic Accounting

### Suspension and retry

A dependency suspension resumes the exact advanced owner. Work performed
before the returned demand or wait is not repeated. A transaction validation
conflict is different: it reconstructs the branch from its committed cut
checkpoint because the branch's observations are no longer serializable.
That replay is required optimistic-transaction behavior, not loss of a WHNF
continuation.

The cut observation journal is cut-wide through suspension. Entering another
`.alt` arm does not discard reads from earlier arms. Branch-local edits remain
discardable, so a failed arm can leave its observations available for conflict
validation without committing its staged diagnostics or store writes.

### Callback and failure ownership

A specialization callback advances its non-cloneable work before returning a
value demand or shared-completion wait. Its host observation or mutation
therefore occurs once even when the requested value later yields, blocks,
fails, or is cancelled. A demanded-value failure returns as rooted
`SpecializationRequestInput::Failed`; it retains the structured diagnostic
value and ordered context stack.

Task reservation, reflection activation, host commits, diagnostic admission,
and dependency subscription remain outside managed value access. When a
machine publishes a blocked state, its complete cut-wide read set has already
been installed and revalidated. A stale post-resumption read therefore causes
an immediate branch restart rather than a permanent missed wakeup.

### Public surface

`SpecializationRequestWait` is still constructed only inside the crate. W5
needed task-join and optimized FIFO waits, but did not invent a general public
host-wait API. That is an intentional boundary, not dead code. Reflection
privilege also does not grant a specialization permission to retain bare core
values across callbacks or waits.

## Forced Verification Matrix

The W5 fixtures use latches, exact phase counters, explicit promise failure,
cancellation, and controlled FIFO publication. Repetition is not used as
evidence for an ordering-sensitive contract.

| Boundary | Evidence |
|---|---|
| original request application pauses before parse and resumes once | `resumable_reflection_decode_consumes_one_application_lazy_after_resumption` |
| suspended and uninterrupted failures preserve identical ordered context | `suspended_request_failure_preserves_context_without_replay` |
| nested reflection in a failed cut arm retires while only fallback output commits | `suspended_nested_reflection_branch_resumes_without_replay_or_leakage` |
| post-resumption FIFO read cannot miss an earlier publication | W5C.5c.4c.1 validation-latch fixture |
| yielded, failed, and cancelled specialization requests do not replay callback preparation | W5C.6b lifecycle fixtures |
| cut-wide observations survive alternative changes | W5C.3/W5C.4 transaction fixtures |

The branch fixture observes the exact parent/child task count before release,
then the exact number of outer application, parse, and dispatch phases after
fallback. The discarded arm's warning never commits, the fallback's
information diagnostic commits once, and all task records retire.

## Drift Assessment

### Intentional and justified

1. **Specializations own request-specific state.** A universal reflection
   continuation would centralize less policy but would either erase typed host
   state or recreate the synchronous callback boundary. The associated type is
   the smaller and safer owner.
2. **Dependency resumption and transaction restart differ.** Exact WHNF work is
   retained for a valid wait; an invalidated optimistic branch is rebuilt from
   its cut checkpoint. Treating both as continuation resumption would accept a
   non-serializable transaction.
3. **The standard specialization uses `Infallible`.** It has no private
   request family, so manufacturing unreachable adapter state added noise and
   weakened the trait contract.
4. **W5 pulled in coordinator subscription repair.** The first complete
   specialization/FIFO schedule exposed a read acquired after the publication
   that woke the machine. Repair at the shared block-publication boundary was
   necessary for the new request owner to be correct and benefits all blocked
   reflection work.

### Future phases

- W6 still owns the remaining builtin evaluator and raw-value conversion, the
  temporary one-ordinary-machine-per-demand-session admission rule, measured
  residual overhead, and the regional standard-effect fusion decision.
- W7 still owns complete recursive-call removal, small-stack verification,
  and a strict shared work budget. Current effect polling is cooperative but
  does not yet establish the final global quantum contract.
- W8 still owns retryable-halt and direct-evaluator compatibility retirement,
  final architecture documentation, and Gate G3 handoff.

The phase boundaries remain coherent, but W6's headings are too broad and its
scope accounting has drifted. The review adds W6.0 as a mandatory preparation
checkpoint.

## Findings

### WHNFW5R-001 — Resolved: structured-context suspension lacked a paired oracle

**Severity:** verification gap

**Status:** resolved during review

The forced failure fixture asserted the expected context list and exact replay
counters, but did not compare that list with an uninterrupted evaluation of
the same source. It now runs both forms and requires identical context order.
This catches a suspension-only context loss or duplication without weakening
the existing exact expected-value assertion.

### WHNFW5R-002 — Resolved: current ownership comments described superseded phases

**Severity:** documentation and auditability

**Status:** resolved during review

The source overview still described the WHNF machine as purely additive, the
reflection agent context described all specialization failures through the
old conversion path, and two protocol comments assigned live behavior to
completed or wrong phases. They now describe specialization-owned work, rooted
demand failure, the remaining bounded compatibility seam, and its W8 owner.
The temporary ordinary-machine admission rule is assigned to W6 closure rather
than to the unrelated operator subsection.

### WHNFW5R-003 — Open: W6 scope is not implementation-ready

**Severity:** high planning and integration risk

**Status:** assigned to W6.0; blocks W6 production edits

The live D.2c inventory contains 176 operations across eight families, while
W6A-W6F are broad prose headings and omit an explicit disposition for the 17
remaining `ValueDemand` violations. W1-W3 completed their resumable
control-flow work, but that does not prove closure of the raw-value API
surface. W6.0 must assign those occurrences and partition every family into
low-risk checkpoints with forced suspension and inventory deltas before
implementation.

### WHNFW5R-004 — Resolved: parent raw-value phase counts predated W5

**Severity:** documentation drift

**Status:** resolved during review

The executable inventory now reports 137 front-end, 26 reflection, and 45
public/compiler/diagnostic violations, while the corresponding active parent
phase descriptions still said 134, 48, and 40. The active descriptions are
updated. The dated D.2a completion record remains historical evidence and is
not rewritten as though those were its original counts.

## Verification

W5D passed the complete routine repository gates before this review:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
scripts/check-interaction-net-profiling.sh
```

The ordinary library run contained 1,628 active tests and two ignored tests at
W5D closure, followed by all integration and executable fixtures. The focused
reflection-machine suite passed 171 tests. WHNF, access, durable-owner, and
raw-value inventories passed.

The three W5D forced schedules also pass individually with
`aggressive-gc-verification`. The complete aggressive suite remains red at the
pre-existing broad public-API compatibility matrix owned by
[`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`](../plans/GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md);
it is neither claimed as W5 evidence nor reclassified as a W5 defect.

Review-close `cargo fmt --check`, all-target/all-feature Clippy, and the full
`cargo test -q` suite pass after the paired oracle and documentation
corrections. The library partition reports 1,628 passed and two ignored tests;
all integration and executable partitions pass. W6 should begin only with
W6.0.
