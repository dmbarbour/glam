# Resumable WHNF W6G.1 Mid-Phase Design Review

Date: 2026-09-19

Scope: W6G.1 of
[`ResumableWhnfEvaluation_2026-09-12.md`](../plans/ResumableWhnfEvaluation_2026-09-12.md)
after W6G.1b, W6G.1e.1, W6G.1f.1, W6G.1f.2a, and W6G.1f.3a.0-a.1.
This is a plan-cohesion review, not the post-W6G.1 implementation audit.

## Outcome

The role-specific pump topology and the separation between foreground roots,
background roots, and demand-driven shared producers remain sound. The main
design defect was applying the lazy-checkpoint model to reflection tasks after
activation. The review also found several places where the plan spoke too
broadly about "every producer" or "one shared checkpoint" and therefore hid
the distinction between resumable computation, autonomous work, and
fire-and-forget orchestration.

The plan has been corrected around three ownership classes:

1. **Demand-driven resumable state.** WHNF, access, object, list, builtin,
   net-WHNF, and isolated net-construction progress belongs in traced managed
   checkpoints when it must survive route loss.
2. **Autonomous root work.** A started reflection task remains a background
   root and fulfills a managed completion source. Its continuation never moves
   into the lazy.
3. **Post-access orchestration.** Host callbacks and spark admission occur
   outside managed access. Host calls require before/after checkpointing
   because no independent root continues them; best-effort sparks require only
   at-most-once scalar admission state in the enclosing builtin checkpoint.

The review covered the current coordinator registries, reflection
reservation/observation path, task settlement obligations and ledgers, spark
release-before-submit path, lazy source/checkpoint transitions, client driver,
causal selectors, explicit drains, quiescence, and all remaining producer
families. Findings below distinguish a design correction from implementation
work which was already intentionally deferred.

## Findings and resolutions

### W6G1R-001 — Reflection was misclassified as a lazy checkpoint

**Severity:** high. **Resolution:** incorporated into W6G.1f.3b.

`ReflectionSourceMachine` currently retains a strong task reservation while
the lazy route waits. Moving that handle into managed checkpoint state would
place a terminal `RuntimeValueRoot` inside the value graph and could form an
uncollectable root cycle. Retaining only the existing weak observation would
instead lose a terminal result after the coordinator retires the task.

The corrected design uses an ordinary managed promise as the exact completion
identity. The reflection task owns the promise's registered producer root only
while active and terminally assigns it. The lazy follows the promise through
the ordinary WHNF checkpoint path. A result which points back to the lazy is
therefore an internal managed cycle after the task releases its roots.

### W6G1R-002 — Terminal promise settlement supports only unresolved failure

**Severity:** medium. **Resolution:** assigned to W6G.1f.3b.0.

Current task-owned promise obligations fail promises left unresolved when a
task terminates. Reflection needs a reviewed terminal mapper: return-value
success assigns the task result, gate success assigns the gate target after
unit validation, and every abnormal disposition assigns its structured
failure. This is an extension of the existing promise settlement boundary,
not a new reflection-specific result archive.

The mapper must preserve the existing diagnostic context and must publish
before the task record retires. Its managed promise root and any temporary
gate-target root are active task obligations, never fields of a managed
checkpoint.

### W6G1R-003 — Reflection launch provenance was underspecified

**Severity:** medium. **Resolution:** assigned to W6G.1f.3b.2.

A canonical lazy may first be discovered by a client, spark, or another
reflection task. Its autonomous reflection child must not inherit that
observer's close policy or private task profile. Admission uses the
runtime-owned pure-production context and selected default reflection profile.
The resulting task is an ordinary executor-visible background root and remains
live after the discovering route or session closes.

### W6G1R-004 — Last-subscriber language incorrectly covered autonomous work

**Severity:** medium. **Resolution:** corrected in W6G.1f and W6G.1f.4.

Last-subscriber retirement applies to transient lazy-producer routes. It does
not cancel an already started reflection task. The route may retire while the
task-owned promise remains an exact completion source. Readiness and explicit
background drains continue to see that task as a background root independently
of any current lazy route.

### W6G1R-005 — Spark publication needed an explicit ownership statement

**Severity:** low. **Resolution:** clarified in W6G.1f.3g.

Spark is best-effort and does not produce the enclosing builtin's result. Once
admitted it is an autonomous background root, but a loss before admission is
permitted. The enclosing managed checkpoint records only enough scalar phase
state to avoid replaying an admission already returned to the coordinator.
The current release-before-submit handoff needs a forced schedule, but it does
not need a transactional outbox or a managed spark checkpoint.

### W6G1R-006 — Family closure wording would remove legitimate task machines

**Severity:** medium. **Resolution:** corrected in W6G.1f.2 and W6G.1f.3i.

W6G.1 removes state-bearing *lazy-producer route* machines. It does not remove
reflection machines from background task records. Those machines are the
authoritative autonomous work. The closure inventory must distinguish route
markers and managed checkpoints from reflection roots and their managed
completion obligations.

### W6G1R-007 — Verification was checkpoint-centric

**Severity:** medium. **Resolution:** corrected in W6G.1f.3b.3, W6G.1f.4,
and W6G.1h.

Reflection verification now counts reservation, activation, and terminal
promise assignment across route loss and first-session closure. It also forces
results and gate targets which point back to the owning lazy, every abnormal
terminal disposition, and a reflection/lazy dependency cycle. Root counts
must return to baseline after terminal assignment and observer release.

### W6G1R-008 — Net construction remains a deliberate design gate

**Severity:** none. **Resolution:** replace the isolated search route through
the W6G.1f.3h
[`PureInteractionNetConstruction_2026-09-20.md`](../plans/PureInteractionNetConstruction_2026-09-20.md)
subplan.

`IsolatedEffectSearch` is demand-driven search state, not autonomous root work
merely because it may launch reflection children. It cannot be replayed after
observable child or journal progress. The existing W6G.1f.3h instruction to
separate traceable search/journal state from orchestration and stop if this
requires a rooted backedge correctly identified the design boundary. The
follow-up audit found that the generic search itself is the unnecessary
boundary: net construction can instead use ordinary pure builder state over
the existing managed `ListEffect` choice/cut machinery, then invoke one hidden
callback-free primitive to validate and replay the uniquely selected strict
netlist. Do not generalize the autonomous-reflection exception to construction
search.

### W6G1R-009 — Failure acknowledgement would be lost behind the promise

**Severity:** high. **Resolution:** assigned to W6G.1f.3b.0 and b.3.

The current reflection source acknowledges a failed task when it directly
propagates that failure. Replacing the direct observation with an ordinary
promise but retaining the task failure ledger would expose one failure twice:
once as an unacknowledged background task and again when a later evaluator
forces the promise.

Assignment is not yet propagation. An autonomous task failure should remain
unacknowledged and reportable if nobody ever observes the promised result.
Instead, extend the promise's existing edge-free producer obligation with
task/session acknowledgement authority, and have the generic failed-promise
propagation path perform the already timing-independent acknowledgement. This
requires neither a task handle nor a registered root in managed memory.

Forced fixtures must cover both outcomes after route loss: an unobserved
failure remains in the ledger, while a later evaluator which propagates the
promised failure acknowledges the ledger entry and emits no duplicate task
diagnostic. Cancellation, abandonment, exit, and kill dispositions retain
their existing terminal reporting policy while still assigning the structured
promised failure selected by the mapper.

### W6G1R-010 — The handoff needed an explicit linearization order

**Severity:** high. **Resolution:** assigned to W6G.1f.3b.1.

Task activation cannot race ahead of promise-producer registration or the
lazy's source-to-WHNF transition. The corrected order roots the source fields,
leaves managed access, reserves the task and installs its terminal mapper,
reenters access to publish the exact promise checkpoint, leaves access, and
only then activates. An activation permit dropped anywhere before activation
terminalizes the promise. If unwind precedes checkpoint publication, the
original source still traces that promise and later demand follows its failure
without reserving another task.

This ordering also supplies the exact task-to-promise causal edge needed by
W6G.1e.2. The same-session reflection fallback must not be retained merely to
cover a gap in handoff publication.

### W6G1R-011 — The cached reflection-observation sidecar becomes vestigial

**Severity:** medium. **Resolution:** assigned to W6G.1f.3b.1 and family
closure.

`ReflectionComputationOwner` and its external-owner `OnceLock` currently keep
a stable reservation discoverable across polls of a route-owned reflection
machine. After the managed source owns its promise and the complete
reserve/publish/activate handoff occurs within one claimed quantum, that
sidecar has no remaining semantic role. Keeping it would preserve weak-task
upgrade failures, an external registry lookup, and two competing notions of
exactly-once ownership.

Remove the owner, handle, and cached observation as part of the handoff. The
managed promise, exclusive lazy source transition, and transient activation
permit become the only exactly-once authorities. Source-backed inventories
must fail if the old sidecar or `LazyTaskWork::Reflection` survives family
closure.

### W6G1R-012 — Session-neutral launch left drain scope ambiguous

**Severity:** medium. **Resolution:** assigned to W6G.1f.3b.2-b.3 and
W6G.1g.

A runtime-owned production session prevents first-observer close policy from
controlling the task, but it also means ordinary session ownership can no
longer answer who is allowed to poll it. Reintroducing the discovering session
as a drain-scope tag would recreate the same affinity under another name and
would be especially arbitrary when several sessions demand the same source.

The task is a runtime-scoped background root. A client or session-local driver
may poll it only by following an exact dependency from work that driver is
authorized to own. Without such a route, workers or the runtime-wide
background drain complete it; an unrelated session drain does not. Forced
zero-worker fixtures must exercise both the exact-dependency path and the
route-loss/runtime-drain path.

## Updated implementation order

1. Implement W6G.1f.3b.0-b.3 before adding another checkpoint family. This
   closes autonomous reflection ownership, failure transfer, and sidecar
   retirement, and supplies exact causal promise edges needed by later
   selector work.
2. Continue demand-driven checkpoint families W6G.1f.3c-g in their dependency
   order. The builtin transition includes the forced spark handoff.
3. Execute the focused W6G.1f.3h pure-construction subplan. Preserve standard
   task-local semantics, but remove the generic isolated reflection search
   instead of attempting to place its external roots in the value graph.
4. Close family inventories in W6G.1f.3i, then perform the route-machine
   cutover and last-subscriber race matrix in W6G.1f.2b.
5. Only afterward install causal background traversal, narrow drains, and
   remove session serialization in W6G.1e.2-e.3/W6G.1g-h.

## Review disposition

No additional blocker was found in W6G.1b, the foreground lifecycle target,
or causal selector ordering. Drain sequencing remains sound after explicitly
classifying autonomous lazy-launched reflection as runtime-scoped rather than
first-observer-scoped work. The two historical parallel failures remain
W6G.1f.2b route/session-affinity evidence; neither is explained away by the
reflection correction. W6G.1f.3h has now passed its design gate with a focused
pure-construction plan rather than by granting the isolated search a rooted
backedge.
