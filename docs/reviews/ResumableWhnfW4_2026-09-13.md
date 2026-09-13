# Resumable WHNF Phase W4 Review — 2026-09-13

Baseline: `3458421`, followed by the W4D inventory reconciliation in this
review.

Status: complete. Every external or pre-existing pollable lazy-source boundary
now has one explicit owner. The review found no open correctness defect or
semantic question which blocks W5. It did catch one invalid test fixture whose
opaque host closure hid a managed value; the fixture now uses the production
explicit-capture protocol.

## Scope

This is the mandatory post-W4 review required by
[`ResumableWhnfEvaluation_2026-09-12.md`](../plans/ResumableWhnfEvaluation_2026-09-12.md).
It audits:

- reflection reservation, activation, completion, acknowledgement, and
  cancellation ownership;
- host callback invocation, result rooting, provenance validation, and
  exactly-once behavior;
- interaction-net and net-construction lifecycle containment after the W4C
  work pulled forward during W3;
- scheduler handoffs and collection safety at every new boundary;
- the residual `produce_lazy_source_in` compatibility family; and
- drift in W5-W8 relative to the implemented W4 design.

## Outcome

`LazyTaskMachine` now selects one typed source owner before executing external
work:

- `ReflectionSourceMachine` owns the exact `ReflectionTaskReservation` after
  first observation. Reservation happens inside the evaluator region, but
  activation is deferred until that region closes. A later poll observes the
  stable wait or its terminal result. Gate completion delegates to the traced
  target; returned-value completion delegates to the coordinator's rooted
  result.
- `HostCallSourceMachine` moves through `Before`, `Invoking`, `After`, and
  `Consumed`. It commits to `Invoking` before entering opaque Rust code outside
  managed access, retains the callback's `RuntimeValueRoot`, and consumes it in
  a later evaluator region. No result shape or failure can replay the call.
- `NetWhnfMachine`, persistent `NetDriver`, and `NetConstructionMachine`
  retain the W4C ownership established during W3. They do not enter the pure
  WHNF frame stack.

The pure `WhnfComputation` therefore still contains only evaluation state and
durable value roots. It has not absorbed reflection-task activation, host
callback lifecycle, effect interpretation, interaction-net claims, or net
driver scheduling policy.

## Ownership Accounting

The enclosing `ManagedLazyRoot` is the durable semantic owner while a source
is pending. For reflection, its managed source traces the effect and optional
gate target. The first-observer permit roots the effect during activation, and
the coordinator's task machine owns it afterward. The source machine itself
retains only the source handle and stable reservation.

For host work, the managed source traces every explicitly declared semantic
capture. `HostCallRootBundle` roots those captures immediately before the
callback and transfers their chosen lifetime to Rust. The returned
same-runtime root is the only semantic value retained by the after-call state.
The callback's external owner remains associated with the managed source and
retires through the existing registry path.

Both source-owner types are private children of `LazyTaskMachine`; neither is
safe or useful without that machine's managed lazy root. Durable-owner and
raw-value inventories now record that relationship explicitly.

## Forced Verification

The new `eval::value::w4_tests` fixtures force, rather than probabilistically
sample, these orderings:

| Boundary | Evidence |
|---|---|
| before callback / after callback / result consumption | `host_call_yields_on_both_sides_and_consumes_its_result_once` |
| callback failure and repeated terminal observation | `failed_host_call_is_not_replayed_after_publication` |
| lazy callback result | `host_call_follows_a_lazy_result_without_reinvocation` |
| reflection recognition / reservation / wait / completion | `reflection_source_reserves_then_waits_from_one_typed_owner` |

The focused tests invoke full collection between returned handoffs. Existing
latched tests continue to cover first-observer/first-activator races,
completion and cancellation during activation, cross-session observation,
structured failures and acknowledgement, runtime mismatch, host contention,
net semantic dependencies, cursor contention, and net-construction
suspension.

During review, the first lazy-result host fixture captured a raw managed
`Value` directly in its opaque Rust closure. Collection correctly exposed the
untraced edge. The fixture now declares that value through
`external_with_semantic_values` and receives it through `HostCallRootBundle`.
This was a test-construction error, not a new production failure, and provides
additional evidence that the existing capture boundary is effective.

## Compatibility Closure

`produce_lazy_source_in` has one executable production family:
`LazySource::Builtin`. `Error` is a terminal-cache invariant check; every other
production variant is routed to a typed owner and represented by an
`unreachable!` arm. `SemanticComputation` and `SemanticThunk` remain explicitly
test-only callbacks.

The source-backed WHNF census decreased by exactly one `ApplyValues` call and
one retryable `EvaluationHalt` construction when reflection moved to direct
reservation/dependency state. The durable-owner, root-publication,
persistent-edge, raw-value, and access inventories have been re-latched after
reviewing those changes.

## Drift Assessment

### Intentional and justified

1. **W4C remained completed ahead of W4A/W4B.** Function-call sources needed a
   persistent net owner during W3. Reopening that work would add no
   verification or ownership benefit.
2. **External boundaries yield more explicitly than before.** Host source
   recognition, callback invocation, and rooted-result consumption are
   separate polls. Reflection source recognition and reservation/activation
   are likewise separate. These are inspectable orchestration boundaries, not
   language-visible sequencing.
3. **Reflection no longer uses retryable-halt transport.** Its typed owner
   returns an exact `WorkDependency` directly. This is the intended direction
   for W8 rather than premature removal of the general compatibility carrier.

### Future phases remain valid

- W5 still integrates resumable WHNF work into the reflection effect machine.
  W4 handles a lazy value whose *source* launches or awaits a reflection task;
  it does not convert reflection request decoding or branch state.
- W6 still owns saturated builtin compatibility and retirement of the
  temporary one-ordinary-machine-per-session admission rule.
- W7 still owns complete recursive-call, stack-size, budget, and fairness
  closure.
- W8 still owns compatibility API retirement and architecture documentation.

## Findings

### WHNFW4R-001 — Resolved: external boundaries lacked typed exactly-once state

**Severity:** high correctness and auditability

**Status:** resolved in W4A-W4B

Reflection previously reconstructed its reservation/poll operation through
the generic source evaluator, while host work retained only a producer pointer
and relied on terminal task behavior to avoid replay. The explicit owners now
retain each lifecycle transition and make duplicate reservation or invocation
constructively impossible.

### WHNFW4R-002 — Resolved: host lazy-result fixture hid a managed edge

**Severity:** test correctness

**Status:** resolved during W4D

The fixture violated the source-backed callback capture contract. It now uses
the explicit capture bundle, and collection between all handoffs succeeds.

### WHNFW4R-003 — Deferred: builtin compatibility remains

**Severity:** planned architecture debt

**Status:** assigned to W6

Saturated `LazySource::Builtin` remains the sole production computation in
`produce_lazy_source_in`. Its recursive evaluator work and the associated
per-session scheduling containment are deliberately handled as one conversion
in W6.

## Verification

Passed during W4 implementation and review:

```text
cargo test -q --lib eval::value::w4_tests
cargo test -q --lib reflection_
cargo test -q --lib host_call
cargo test -q --lib whnf_inventory
cargo test -q --lib access_inventory
cargo test -q --lib net_driver
cargo test -q --lib net_construction
cargo test -q --lib inventory
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q --lib
cargo test -q --lib --features aggressive-gc-verification eval::value::w4_tests -- --test-threads=1
```

The ordinary library suite passed with 1,569 tests and 2 ignored. The complete
aggressive library suite was also attempted, but remains red across the broad
API/evaluator/reflection fixture matrix already assigned to the active
[`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`](../plans/GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md)
plan; Gate G3 explicitly remains closed there. It is not W4 closure evidence.
The W4 forced-handoff suite itself passes in aggressive mode, and explicitly
collects between every new boundary in both modes.

The required `cargo test -q` command was also run. Its library and intervening
integration suites passed, but four pre-existing direct-assembly executable
fixtures remained CPU-bound after 12 minutes 49 seconds and the run was
terminated without a terminal result. This is the separate slow-fixture issue
already tracked from W3, not substitute evidence for W4 correctness; no test
had failed before the slow tail.
