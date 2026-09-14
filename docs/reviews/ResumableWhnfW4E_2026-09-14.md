# Resumable WHNF Phase W4E Review — 2026-09-14

Baseline: `3458421` for the original W4 review; W4E implementation and
verification span `cb87515` through `028a215`.

Status: review complete. The deterministic semantic replay defect recorded as
`WHNFW4R-004` is resolved. W5 may begin. One bounded performance regression
remains assigned to W6G and is not a correctness blocker.

## Scope

This is the post-remediation review required by W4E.4 in
[`ResumableWhnfEvaluation_2026-09-12.md`](../plans/ResumableWhnfEvaluation_2026-09-12.md).
It supplements rather than rewrites the original
[`ResumableWhnfW4_2026-09-13.md`](ResumableWhnfW4_2026-09-13.md) review. It
audits:

- the W4E accounting tool and its ordinary-build boundary;
- the cause and repair of function-call semantic replay;
- net claim, normalization, source, cache, and client-demand ownership across
  interruption and completion;
- whether W5-W8 still describe the work remaining after the repair; and
- the evidence used to close the W4 performance blocker.

## Outcome

`CoreOperator::ApplyArity` previously ran a saturated application while its
operator pair remained claimed. If over-application first produced a lazy
function and nested WHNF evaluation yielded, restoring the pair discarded that
intermediate application state and replayed the semantic call.

The operator now commits an ordinary application lazy. Its WHNF computation
owns the remaining argument cursor and resumes independently of the completed
operator reduction. This changes the internal net topology slightly but
preserves the language result and makes source selection and application
progress durable.

The `interaction-net-profiling` feature supplies two separate observations:

- committed rule-family counts, which are the semantic replay oracle; and
- driver counts, which explain order-sensitive cursor and scheduling work.

Ordinary builds contain neither counter storage nor update hooks. A test-only
work fuse exists only in profiling unit-test builds and yields after a closed
normalization batch or semantic step; it is not a runtime policy or Glam
failure.

## Ownership and Interruption Audit

The minimized wrapper/returned-function fixture completes in 17 coordinator
polls with 71 driver work items and the latched rule signature. At completion:

- its client-demand record is retired;
- its source is removed and terminal cache is present; and
- its source net retains neither an in-flight claim nor a normalization lease.

The inverse fixture stops the same computation after exactly 16 work items. At
that boundary the demand remains pending, the source remains installed, no
terminal cache exists, and the source net again retains no claim or
normalization lease. Abandoning the handle removes the remaining client-demand
record. The cutoff therefore observes a resumable owner rather than a partial
terminal publication.

## Drift Assessment

### Intentional and justified

1. W4E repaired the defect with a durable application lazy rather than
   restoring recursive evaluation inside a claimed pair. This follows the
   resumable-WHNF direction even though it adds a small amount of net topology.
2. The provisional source-level verification matrix was narrowed after the
   defect reduced to a deterministic semantic fixture. Exact semantic and
   driver signatures plus direct lifecycle inspection are stronger evidence
   for this boundary than another complete assembly harness.
3. No forced zero/one-worker schedule was added. The defect did not depend on
   cross-thread ordering; existing worker and restoration suites continue to
   own those independent contracts.

### Future work remains coherent

- W5 still hosts resumable WHNF inside reflection request decoding.
- W6 still removes builtin/direct-evaluator compatibility and now owns the
  measured residual overhead.
- W7 still implements the real task budget and fairness policy. W4E's fuse is
  only a deterministic test oracle for a net-work boundary.
- W8 still retires recursive-halt/direct-evaluator compatibility, while now
  explicitly preserving durable over-application state.

## Findings

### WHNFW4ER-001 — Resolved: claimed over-application replayed semantic work

**Severity:** high correctness and performance

**Status:** resolved by W4E.2

The last-known-good semantic fixture exposed unbounded replay immediately. The
durable application owner restores bounded execution and the exact result.

### WHNFW4ER-002 — Deferred: resumable net scheduling retains bounded overhead

**Severity:** medium performance

**Status:** assigned to W6G

The duplicate-symbol assembly fixture now completes in approximately 12.6 to
12.9 seconds rather than the pre-W4 approximately 8.1 to 8.5 seconds. Driver
work is comparable, so this is no longer semantic replay or superlinear
scheduler growth. W6G measures machine polling, managed-access, claim, and
requeue costs as compatibility boundaries disappear.

### WHNFW4ER-003 — Resolved: profiling behavior lacked a routine focused gate

**Severity:** low verification process

**Status:** resolved by `scripts/check-interaction-net-profiling.sh`

Clippy compiled the profiling feature, but the ordinary suite did not execute
its feature-only behavior. The focused script names the six feature-only tests
and the one profiling-augmented semantic fixture without repeating the entire
ordinary suite under instrumentation.

## Verification

The following gates pass at review close:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
scripts/check-interaction-net-profiling.sh
```

The ordinary suite passes 1,570 active library tests plus all integration and
executable fixtures. The source-shaped duplicate-symbol fixture emits its one
expected structured diagnostic and completes in 12.88 seconds in the final
W4E verification run.
