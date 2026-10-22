# Poll Budget — 2026-10-22

An audit of how polls spend and report their step budget, and the steps that
bring them back under `reduction-costs-one-budget-unit`. Prompted by the
maintainer on 2026-10-22, after `api-stack-ops` found the demand pump
charging whole quanta. Closes holistic review E1 (S1 item 2), open since
2026-10-03, though thought closed before performance work began.

## The contract

- **Coarse-grained steps, bounded by the budget** (maintainer, 2026-10-22,
  as consistently before; until budgets existed, "evaluate until done or
  blocked"). `coarse-grained-mutator` and the fused handling of pure
  effects serve the same end. Nothing but the budget ends a poll that
  could go on.
- **A poll runs until it completes, fails, blocks, or spends its budget.**
  The outcome `Yielded` is renamed `BudgetExhausted`, so its scope stays
  plain; wherever a poll ends, it implies `remaining() == 0`, checked by a
  debug assertion (maintainer, 2026-10-22). A handoff, where one must be
  told apart, gets its own internal name.
- **A step that changes state pays one unit**, taken where the state
  changes, and needs a unit available to start. Observing a value or a
  wait, walking a route, and blocking are free.

"One step per poll" was never the design. The architecture docs describe
what is, and the phrase entered `architecture/evaluation.md` with
`c6373546` (2026-10-05), describing what `machine_step` then did; the
step-at-a-time machines it described predate it.

## Findings

### Yields with budget left

The WHNF driver (`eval/whnf.rs`) and the net driver's rule claims keep the
contract. Everything else yields for other reasons too.

1. **Phase handoffs: "I took a step, poll me again".**
   - Regional machines take one step per poll
     (`docs/architecture/evaluation.md` "a machine takes at most one step
     per poll"). Comparison yields after every frame; access, list,
     annotation, dict, pattern and object machines yield after each
     member, item or frame. Roughly a hundred sites.
   - The lazy task machine yields at its own checkpoint swaps: family to
     WHNF, `Replaced`, `resume_moved`, forward to produce, and the host
     call's invoke phases (`eval/value.rs`).
   - The net driver restarts from its request root after `Progressed`, then
     yields (`eval/net.rs`).
   - Reflection's child decoders (list front, key conversion, value path)
     yield after each structural step, where their parents use `Continue`
     for the same kind of change (`reflection/machine.rs`).
2. **Waiting on another thread, reported as a yield.**
   - `Contended` queues its wait for after admission and yields.
   - `SemanticInFlight` yields to spin until another session's driver
     finishes.
   - Deferred work ends the quantum (`quantum-mutator`), as a yield.
3. **`ScheduleSpark` ends the poll** as a yield, though the spark could be
   queued as boundary work and the poll continue.
4. **Sentinels.** `offer_inline` and `report_completion` overload `Yielded`
   to signal the route loop, which intercepts them.
5. **Reflection misreports.**
   - A pass that only blocked is charged a unit (`poll_pumping`), so at the
     last unit the pump finds no allowance and the poll reports `Yielded`
     rather than `Blocked`.
   - A failed step whose retry restarts the task yields
     (`install_blocked(..).unwrap_or(Yielded)`).

### Workarounds these yields need

- **`charge_if_unchanged`** charges one unit when a step spent nothing.
  - Origin: `2f07d78a` (W6B.0, 2026-09-16). Reflection moved from a fresh
    `max(1)` budget per phase pass to one shared budget, and the charge
    "preserves one-unit progress without renewing fuel": it stands in for
    the old pass counter.
  - Reused in `machine_step` (`c6373546`, which wrote the ADR's builtin
    bullet), in `poll_pumping` (`4a0fa1b3`, when pumps still minted fresh
    allowances) and in the task poll (`df246e5a`, moved by `b124aaa6` to
    the runtime pump's `.max(1)`).
  - Its role: a step that hands off without spending would otherwise loop
    without bound. Two uses carry real weight: `poll_pumping`'s charge is
    the only bound on its re-poll after `TargetReady`, and a host request
    answering `Continue` forever would spin without it.
- **`IDLE_YIELD_REPOLLS = 8`** and the "repoll if the budget fell" rule
  (`d77192b1`, `inline-lazy-forcing`) tell a handoff from a spin by
  counting, since a handoff yield does not spend.
- **The net test probe drains its whole budget** (`d77192b1`) so the
  repolling route stops at it.
- **The runtime pump's `.max(1)`** (`b124aaa6`) counts a poll that reduced
  nothing.
- **Test machines pay a unit per step** (`b124aaa6`). This one stays: it
  is the contract.

### Artificial limits on efficient evaluation

Beyond the yields above, which each cost a scheduler round trip:

- **`TASK_POLL_QUANTUM = 64`** caps every poll at 64 units, undocumented.
  The demand pump lends at most 64 per poll even from a larger budget, and
  so do the client driver and the causal-child poll. Workers need some
  quantum, for fairness and for collections that join at quantum
  boundaries, but a demand-driven poll could take what its caller lends.
- **Chunked grants of 4,096** (`AWAIT_PUMP_ALLOWANCE`, the client driver's
  pump grant, `BACKGROUND_DRAIN_BUDGET`, the reflection effect loop's): with
  exact charging these only split work into calls, each returning to its
  caller's loop.
- **Each machine step re-enters its access region**, checkpoint lock and
  managed-edge transition (holistic review E2), since a step returns to the
  route loop. Looping within the poll leaves one region per poll.
- **`INLINE_LAZY_DEPTH = 32`**: deeper forcing spills to a route,
  documented as bounding the work a later poll repeats. Kept unless a
  profile shows spills.
- **Deferred work ends the quantum** (`quantum-mutator`), from before
  `coarse-grained-mutator` let a poll suspend in place.

Not limits on evaluation: `WHOLE_COPY_MAX_NODES` and
`NORMALIZATION_MAX_NODES` are representation and compile heuristics with
their own decisions; `LIST_OPERATOR_MAX_ARITY` is language;
`EFFECT_FUSION_BUDGET` is test-only.

### Suspected bugs

- **A reflection task blocked on a panicked wait spins.** `poll_blocked`
  reports `Blocked` for a panicked dependency, so the scheduler's panic
  boundary halts the task. But `poll_pumping` pumps the wait first, the
  pump reports a panicked wait as `TargetReady`, and the loop polls again.
  Only the charge per pass ends it, as `Yielded`; the scheduler never sees
  the block, requeues the task, and the next poll spins again. Untested.
- **A compile batch can run free.** `'compile` of an evaluated function
  spends its admission unit first, then charges the batch clamped to what
  remains, so with one unit the whole normalization costs one.
- **Uncharged state changes:** a compiled application (`apply_compiled` in
  `install_source_in`) and a host call's invocation change state with no
  unit.

### Dead code

- `ClaimedTask::release`'s `made_progress` and `remains_blocked`: every
  caller discards them.
- The boundary arms after `interpret_poll` of a deferred request, which
  returns only `Pending` or `Failed` (`eval/value.rs`).
- `poll_pumping` pumps a wait that `poll_blocked` already pumped.

## Steps

Bug first, then the machines from the inside out, then the assertion that
proves it.

1. `budget-panicked-wait` — a test that forces a reflection task to block
   on a panicked wait through `poll_pumping`, then the fix: check a
   panicked dependency before pumping, beside the recursive-promise check,
   and pump once.
2. `budget-machine-steps` — regional machines continue within a poll after
   a step, and `machine_step` loops while a unit remains, paying one unit
   per step at its start; one access region spans the loop.
   `charge_if_unchanged` leaves `machine_step`, the regional polls' yield
   becomes `BudgetExhausted`, and the text "one step per poll" goes.
3. `budget-lazy-route` — the lazy task machine loops over its own handoffs
   within a poll, with an internal step result in place of the sentinels;
   the net driver restarts within the poll after `Progressed`.
   `IDLE_YIELD_REPOLLS`, its repoll rule and the probe's drain go.
4. `budget-waits-block` — `Contended`, `SemanticInFlight` and deferred work
   end a poll as what they are rather than as a yield, or run suspended in
   place where `coarse-grained-mutator` allows; a spark is queued as
   boundary work and the poll continues.
5. `budget-reflection-phases` — reflection's decoders continue within the
   poll; each phase advance pays where it happens, a host's `Continue`
   included; `poll_pumping`'s charge goes, a restarted retry continues,
   `UnitEffectTask` polls through `poll_pumping`, and the effect polls'
   yield becomes `BudgetExhausted`.
6. `budget-exhausted-only` — the scheduler's `Yielded` becomes
   `BudgetExhausted`, and the assertion that it leaves no budget holds
   where task, client and regional polls end. The runtime pump's `.max(1)`,
   `charge_if_unchanged`, the dead outputs and arms, and the "reservation"
   wording go.
7. `budget-batch-admission` — a batch is admitted while a unit remains and
   charges at least one; compiled application and host invocation pay a
   unit.
8. `budget-quantum` — measure, then size the task quantum for workers, let
   demand-driven polls take what their caller lends, and drop the chunked
   grants where a caller has no budget of its own.

Then: amend `reduction-costs-one-budget-unit` (what `Yielded` means, what
a machine step costs) and `step-budget-is-reservation`; close E1 in the
holistic review; update `architecture/evaluation.md` and
`agent_context/evaluation.md`, which still state the administrative unit.
Exact-budget fixtures are re-baselined deliberately, as E1 asked.

Not here: `anno 'normalize` walks again from its root on every poll
(`3f76f27b` kept "its budget-charged form for now"). It still progresses,
since its reductions stay in the net, so it belongs on the performance
roadmap.

## Open questions (2026-10-22)

Taken as recommended unless the maintainer says otherwise; the maintainer's
answer on 2026-10-22 ("coarse-grained steps bounded by budget") fits the
first.

1. **What a machine step costs.** Recommended: one unit per step, always,
   paid at its start; delegations within it pay their own. Today a step
   pays only when its operands cost nothing. Walking evaluated data stays a
   paid step rather than free observation, because the budget is what
   bounds how long a poll holds its mutator (`coarse-grained-mutator`).
2. **How a poll ends for work it must wait out.** Contention, an in-flight
   semantic action, and work deferred past admission each need another
   thread or the mutator's release. Options: report each as `Blocked` on a
   dependency that wakes it, or add one outcome for "ended at a boundary
   with budget left". Blocking is the closer fit where a wake source
   exists.
3. **Whether a host call's invocation and a compiled application are
   reductions.** Recommended: yes, one unit each, as an immediate builtin.

## As built

- `budget-panicked-wait` (2026-10-22): confirmed first by
  `an_effect_blocked_on_a_panicked_dependency_halts_instead_of_spinning`,
  where sixteen pump calls of 4,096 steps each spent their budgets on the
  panicked wait. `poll_pumping` now returns a block on a panicked
  dependency without pumping it, and the scheduler halts the task with the
  panic. The pump that `poll_blocked` and `poll_pumping` both run stays
  for `budget-reflection-phases`.
- `budget-machine-steps` (2026-10-22): the regional machines' polls and the
  sub-machines beneath them (key and path conversion, list fronts and
  backs, tagged payloads, object linearization, comparison frames, net
  construction) report `Continue` after a step of their own and
  `BudgetExhausted` only when the budget is spent; WHNF's polls only ever
  exhaust, so they were renamed outright. `machine_steps` loops at the
  checkpoint, under one access region and one checkpoint transition, until
  anything but `Continue`. A step needs a unit to start and pays one as it
  ends, forgiven when its operands spent the last: paying as it started
  left a one-unit poll nothing for its operands, and three reflection tests
  that poll one unit at a time spun. Reflection and the spark path still
  treat a `Continue` from the durable conversions and the strategy machine
  as a yield, until `budget-reflection-phases` and
  `budget-exhausted-only`.
