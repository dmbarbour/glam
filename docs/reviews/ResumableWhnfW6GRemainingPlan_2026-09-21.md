# Resumable WHNF W6G.1 Remaining-Work Plan Review — 2026-09-21

Status: forward-plan review complete; W6G.1 implementation remains open.
Parent plan: [Resumable WHNF evaluation](../plans/ResumableWhnfEvaluation_2026-09-12.md).
The [interim implementation review](ResumableWhnfW6GInterim_2026-09-21.md)
covers what has already landed. This review checks the *remaining* W6G.1 order,
dependencies, and checkpoint sizes against that implementation. It neither
reopens completed checkpoints nor replaces the mandatory post-W6G review.

## Current-state evidence and drift judgment

The next unfinished producer-family checkpoint is **W6G.1f.3i**, not f.4.
[`LazyTaskWork::Whnf`](../../src/eval/value.rs) still carries a computation
in the deferred route for application, function-fixpoint, static-access,
immediate-builtin, and test-only callback paths. The managed checkpoint
carrier already covers the migrated families. This is intentional staging;
the old execution-order summary was nevertheless misleading because it
placed the cross-cutting W6G.1c contract in a linear sequence and did not
state that f.3i must precede f.2b and behavioral f.4 verification.

The coordinator still keeps a boxed producer machine in
[`DeferredWork`](../../src/evaluation/coordinator/deferred.rs) and the first
observer's `demand_session` in its enclosing
[`WorkRecord`](../../src/evaluation/coordinator.rs). Its claim, release, and
close paths therefore cannot be made session-neutral
by merely changing a selector. A task-owned promise producer must retain its
machine and terminal obligation; the machine-free cutover is specifically for
lazy routes. The representation choice between variants of a shared record
and separate route records remains an implementation decision for f.2b.0,
not a hidden decision made by this review.

[`drive_client_demand`](../../src/evaluation/session.rs) exact-claims the
private foreground record but still invokes runtime-wide help when blocked.
[`pump_demand`](../../src/evaluation/pump.rs) still has a same-session ready-task
fallback. [`select_worker`, `select_runtime_pump`, and
`session_has_running_machine`](../../src/evaluation/coordinator.rs) still
reflect global deferred selection and temporary session serialization.
Those are coherent transitional policies. Removing them ahead of explicit
causal reflection-child edges would risk losing real progress, while treating
them as final policy would violate the intended foreground/background split.

## Findings and plan resolutions

| Finding | Judgment | Resolution in parent plan |
| --- | --- | --- |
| W6GRP-001 — execution order skips open prerequisites | Documentation drift, not code defect. f.3i and f.2b must precede behavioral f.4; W6G.1c is an acceptance target rather than an extra step. | Replaced the coarse order with the completed baseline and six dependency steps; called out f.3i.0 as the next implementation checkpoint. |
| W6GRP-002 — f.2b is too large and contains an unresolved route-shape choice | Material implementation risk. One generic deferred record currently serves both lazy routes and task-owned promise production. | Added f.2b.0 representation/lifecycle gate, typed groundwork, coherent cutover, last-subscriber retirement, and forced regression gate. Retain the option to merge admission/release code if splitting creates an unsafe intermediate state. |
| W6GRP-003 — foreground and background policy are interdependent | Ordering drift. Exact foreground claims exist, but the driver and drain still depend on broad work selection. | Split d into ownership audit and exact-only driver; split e.2 into traversal preparation, coherent selector cutover, and forced matrix; split e.3b into child-edge inventory, publication, and fallback removal. |
| W6GRP-004 — drain and serialization duties overlap | Duplicate implementation risk, not a second semantic policy. | Made e.3a an evidence gate for g, and e.3c an audit/handoff to h. Split g by session drain, runtime drain, and readiness; split h by census, policy removal, and final verification. |
| W6GRP-005 — family and retention checkpoints mix distinct evidence | Checkpoint-size and acceptance-order risk. f.3i.1's three direct WHNF sources currently share a constructor shape, so a mandatory split would be premature; builtin results and test-only callbacks do not share that boundary. | Kept f.3i.1 grouped with a conditional split, divided f.3i.2 into production builtin and test callback steps, and allowed f.4a's census early while gating f.4b-d on the new route lifecycle. |

## Recommended near-term path

1. Do f.3i.0's source/route census, then f.3i.1, f.3i.2a-b, f.3i.3, and
   f.3i.4. Verify payload *shape*, not just enum variant names, before
   declaring every route marker state-free.
2. At f.2b.0, choose and document the lazy-route/task-owned-promise record
   distinction. Implement f.2b.1-.4 with latched first-session-close,
   publication, final-subscriber, and new-subscriber orderings.
3. Complete f.4's integrated retention and mixed-observer evidence. Its
   census may be prepared in parallel with earlier work, but it must not
   certify the transitional coordinator lifecycle.
4. Inventory and publish causal child work, then cut background selectors
   and remove same-session fallback. Only then finish the exact foreground
   driver, narrow drains, and retire the temporary serialization scan.

The selector cutover is intentionally one coherent mechanism checkpoint:
workers and runtime background pumping must agree on which deferred work is
causally eligible. The f.2b admission/poll/release cutover may also require
one implementation commit, but its representation gate and forced verification
are separate low-risk checkpoints. No new public `Evaluation`/`try_advance`
API is required to complete W6G.1.

This is a plan/documentation review only. It changes no Rust behavior and
does not claim the future forced schedules have passed.
