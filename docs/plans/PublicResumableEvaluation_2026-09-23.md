# Public Resumable Evaluation Plan

Date: 2026-09-23

Status: preliminary and deferred until the resumable-WHNF W6G performance
closure. This plan consolidates an API transition which was previously
mentioned only as deferred follow-up in the resumable-WHNF plan and reviews.

## Purpose

Library clients currently demand weak-head normal form through the blocking
operation:

```rust
ValueEvaluator::eval(&Value) -> Result<EvaluatedValue, Error>
```

Internally, evaluation is already resumable. A private `ClientDemandHandle`
owns the foreground demand record and completion cell, bounded pumps accept an
evaluation-step allowance, an `ExactDemandRoute` avoids rediscovering an
unchanged producer chain, and the coordinator provides a lost-wakeup-safe
generation wait. The blocking driver keeps this state only in one Rust call.

Introduce a public, opaque evaluation handle which lets a library client
advance the same computation in bounded increments, retain it while doing
other work, wait without spinning when another owner has the useful claim, and
resume without replaying completed semantic work or rediscovering a stable
exact route. Preserve blocking `eval` as the ordinary convenience API.

The public names and exact Rust enum layout remain provisional. Resumption is
not provisional: after a bounded return, advancing the same handle must
continue the same admitted client demand and retained scheduler route.

## Current Foundation

The transition should expose orchestration already present rather than add a
second evaluation mechanism:

- `ClientDemandHandle` owns one client-demand record, its completion cell, and
  explicit abandonment on drop.
- `ExactDemandRoute` is a private, non-authoritative scheduler zipper. It
  contains only work identities and validation tokens and survives bounded
  calls when its caller retains it.
- route-aware pumping distinguishes terminal progress, budget exhaustion,
  another owner's busy claim, and stable lack of an exact producer.
- the coordinator's generation/predicate wait registers and rechecks under
  the publication mutex, preventing a completion between observation and
  sleep from being lost.
- `EvaluationRuntime::pump_background` separately advances bounded
  runtime-visible reflection work. It deliberately excludes foreground client
  roots and sparks.

No public `Evaluation` or `try_advance` API exists today. The current
`ValueEvaluator::eval` admits a private demand and drives it to completion in
one blocking call.

## Provisional API Shape

The first design checkpoint should refine an interface with approximately
this shape:

```rust
impl<'runtime> ValueEvaluator<'runtime> {
    pub fn start(&self, value: &Value) -> Result<Evaluation<'runtime>, Error>;
}

pub struct Evaluation<'runtime> {
    // opaque: matching assembler/runtime service, ClientDemandHandle,
    // ExactDemandRoute, and the last wait observation
}

pub struct EvaluationBudget {
    // an evaluation-transition allowance, not wall-clock time
}

pub enum EvaluationAdvance {
    Complete(EvaluatedValue),
    BudgetExhausted { spent_steps: usize },
    Waiting { spent_steps: usize },
}

impl Evaluation<'_> {
    pub fn try_advance(
        &mut self,
        budget: EvaluationBudget,
    ) -> Result<EvaluationAdvance, Error>;

    pub fn wait_for_change(
        &mut self,
        timeout: Option<std::time::Duration>,
    ) -> EvaluationChange;
}
```

This is a semantic sketch, not an API commitment. In particular, the design
checkpoint must decide whether terminal completion consumes the handle,
whether the budget is a public newtype or a numeric argument, and whether the
wait result and timeout live on `Evaluation` or a separate opaque waiter.
None of those choices may weaken resumability or expose private work IDs,
subscription epochs, dependencies, or coordinator generations.

## Semantic Invariants

1. **One admitted computation.** `start` admits one foreground client demand.
   Every later advance continues that record until completion, explicit
   cancellation, or abandonment. A bounded return never reconstructs the
   source computation.

2. **Retained exact progress.** The handle owns its `ExactDemandRoute` beside
   the `ClientDemandHandle`. Budget exhaustion, a busy producer, and a timed
   wait preserve both. Semantic values, lazy cells, runtime nets, and shared
   coordinator records do not acquire client scheduler routes.

3. **Affine advancement.** At most one thread advances a handle at a time.
   The initial API should express this with `&mut self`; it must not introduce
   an internal mutex merely to permit concurrent polling. Moving an inactive
   handle between threads may be supported if the existing runtime
   capabilities safely satisfy `Send`, but that is an explicit trait-contract
   checkpoint rather than an accidental auto-trait promise.

4. **Budget means admitted transitions.** A zero budget claims and polls no
   work. A positive budget is shared through nested evaluator work and reports
   the amount spent. A host callback or other already-entered indivisible
   transition cannot be preempted halfway merely because its surrounding
   allowance expires.

5. **No hidden productive-wait policy.** `try_advance` claims only the
   foreground record and work reachable through its exact producer or causal
   child routes. It does not pump unrelated reflection work, sparks, or
   same-session tasks. A client may explicitly compose `try_advance`,
   `EvaluationRuntime::pump_background`, and a timed change wait.

6. **Waiting is wake-safe and opaque.** A `Waiting` return means no exact work
   was locally claimable at the observed instant. Waiting on the handle must
   recheck completion and coordinator change under the existing publication
   protocol. It exposes no private identity and may return immediately if the
   relevant state changed before the call.

7. **Busy is resumable, not absence.** If another thread owns a useful exact
   producer, the handle retains that producer as its retry point and reports a
   waitable state. It must not turn contention into a stable evaluation
   failure or broaden its authority to unrelated work.

8. **Terminal errors remain structured.** An evaluation failure is converted
   through the same assembler/runtime diagnostic boundary as blocking
   `ValueEvaluator::eval`. A bounded interface does not create a second error
   representation. The design checkpoint must specify repeated observation
   of terminal state or make terminal extraction consuming.

9. **Runtime provenance remains enforced.** The input `Value`, handle, result,
   and eventual `EvaluatedValue` belong to one `EvaluationRuntime`. The public
   facade does not make values transferable between runtime domains.

10. **Dropping a client is not background transfer.** The provisional default
    is the current `ClientDemandHandle` behavior: dropping an unfinished
    `Evaluation` abandons only that client root and its subscriptions. A
    canonical lazy producer demanded by another client, reflection task, or
    spark remains live. Silently making arbitrary foreground work worker-
    eligible is forbidden. Explicit cancellation or transfer-to-background
    can be considered as later APIs, but must not be implied by `Drop`.

11. **One evaluator path.** After cutover, blocking `ValueEvaluator::eval`
    must be implemented by admitting the public handle and repeatedly using
    the same advance/wait machinery. Blocking and bounded evaluation may
    differ in orchestration policy, not in semantic evaluation behavior.

## Non-goals

- serializing or persisting a suspended evaluation;
- cloning or concurrently polling an evaluation handle;
- exposing coordinator work IDs, lazy IDs, promise IDs, route frames, or
  private deadlock topology;
- making foreground evaluation automatically drain background reflection;
- transferring unfinished client demand to workers on ordinary drop;
- wall-clock preemption of an evaluator transition or host callback;
- redesigning internal WHNF, reflection, promise, lazy, or net machines; and
- selecting a general progress-callback or async-runtime integration API.

An async adapter may later be built from the same opaque change-wait
primitive. This plan should not tie the base library to one async executor.

## Transition Phases

### PRE-0 — Revalidate the boundary

- Re-audit `ValueEvaluator::eval`, `ClientDemandHandle`, the blocking driver,
  route-aware bounded pumping, timed coordinator waits, and
  `pump_background` after W6G closes.
- Confirm the W6G4R-001F lifecycle and invalidation fixtures still exercise
  the exact mechanisms the public facade will retain.
- Decide the public lifetime and trait contract. Prefer a handle borrowing the
  matching assembler/runtime service unless an owned service handle has a
  demonstrated use case.
- Freeze provisional public names only after writing compile-level examples.

### PRE-1 — Internal nonblocking advance

- Split one private advance operation from the blocking driver. It accepts a
  mutable client handle, mutable exact route, and caller-supplied step budget.
- Return terminal result, exhausted allowance, or a wake-safe blocked/busy
  observation without waiting.
- Preserve the terminal publication-gap handling: disappearance of the
  coordinator record before result-cell publication is not completion or
  stable absence.
- Keep the existing blocking driver as a composition over this operation
  during the transition.

### PRE-2 — Public handle and budget surface

- Add `ValueEvaluator::start` (or the selected equivalent) and the opaque
  `Evaluation` handle.
- Retain the matching runtime/assembler capability, private demand handle,
  exact route, and wait observation together.
- Add bounded `try_advance`; guarantee zero-budget no-op behavior and report
  spent steps for nonzero budgets.
- Map completion to `EvaluatedValue` through the existing runtime observer and
  errors through the existing structured diagnostic enrichment.
- Add compile-time trait assertions for the selected `Send`, `Sync`, `Clone`,
  and equality contracts. The expected initial contract is affine,
  non-`Clone`, non-equality-bearing, and not concurrently pollable.

### PRE-3 — Waiting and lifecycle

- Add the opaque lost-wakeup-safe change wait, including an optional timeout
  that applies only while no thread is executing the handle's work.
- Specify and implement terminal-call behavior, explicit abandonment or
  cancellation if exposed, and ordinary `Drop`.
- Ensure a retained inactive handle keeps its client record/subscriptions
  available for later resumption without keeping unrelated sessions or work
  alive.
- Confirm dropping one observer does not discard a canonical producer still
  demanded elsewhere.

### PRE-4 — Rebase blocking evaluation

- Implement `ValueEvaluator::eval` over the same start/advance/wait protocol.
- Preserve its current external behavior and structured errors.
- Remove the old independent blocking loop after all internal and public
  callers use the shared mechanism.
- Keep background pumping an explicit host decision; the blocking convenience
  must not silently regain the retired runtime-wide helping policy.

### PRE-5 — Verification and documentation closure

- Document the public lifecycle and examples in the library API and the
  appropriate user-facing Rust integration documentation.
- Update evaluation architecture docs to distinguish semantic computation,
  the client-owned public handle, and runtime-background work.
- Audit every public and crate-private evaluation entry so only deliberate
  blocking conveniences bypass direct handle ownership.
- Measure one-shot blocking evaluation before and after rebasing it. The
  facade should not add per-transition root registration, route rebuilding,
  or synchronization beyond the existing client record.

## Forced Verification Matrix

Tests must force event order with barriers, probes, or explicit claims.
Repeating a concurrent test is not evidence.

### Budget and resumption

- zero budget performs no transition and preserves an untouched handle;
- one-step advances reach the same result and structured failure as one large
  budget and blocking `eval`;
- budget exhaustion after nested progress resumes from retained WHNF state;
- repeated bounded calls retain the exact route and do not repeat the initial
  cold producer-chain traversal; and
- an indivisible callback may exceed the surrounding wall-clock expectation
  but is charged as the evaluator's existing transition contract specifies.

### Claim and wake ordering

- producer completes before `try_advance` observes its wait;
- producer completes after observation but before `wait_for_change` sleeps;
- producer completes while the handle is sleeping;
- another thread claims the exact producer, then publishes or releases it;
- a parent is woken by another completion source and later blocks on a new
  dependency; and
- timeout returns without changing or abandoning the computation.

### Ownership and drop

- dropping queued, blocked, and waitable handles removes only their client
  roots and subscriptions;
- dropping after terminal publication neither loses nor duplicates the result;
- retaining a handle across unrelated host work resumes the same demand;
- another client or reflection root keeps a shared lazy producer live after
  the first handle is dropped; and
- the public handle cannot be cloned or polled concurrently under the selected
  trait contract.

### Composition

- foreground `Waiting`, bounded `pump_background`, and timed waiting compose
  without spinning and without either pump claiming the other's roots;
- background absence does not turn a foreground wait into failure;
- blocking `eval` and incremental evaluation produce equivalent valid values;
  and
- runtime-provenance mismatch is rejected before admitting work.

## Exit Criteria

- A library client can retain an unfinished evaluation, advance it with a
  bounded transition allowance, wait without a lost wakeup, and resume the
  same admitted work.
- Exact-route and WHNF progress survive every ordinary bounded return.
- Blocking `ValueEvaluator::eval` is implemented over the same mechanism.
- Drop and cancellation behavior are explicit and order-forced tests prove
  that shared producers and unrelated work are unaffected.
- The public surface exposes semantic outcomes and opaque waiting only, not
  bootstrap scheduler identities.
- Full formatting, lint, ordinary tests, profiling tests, and any applicable
  Loom/Miri gates pass.

