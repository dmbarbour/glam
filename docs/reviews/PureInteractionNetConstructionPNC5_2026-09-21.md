# Pure Interaction-Net Construction PNC5 Review — 2026-09-21

Baseline: `26675624` completes PNC5A.0–D. The legacy construction producer is
still compiled but is no longer selected by the public `interaction_net`
builtin; its removal is PNC6 work.

Status: review and remediations complete on 2026-09-21. No semantic result
defect was demonstrated. The pure cutover has the intended ownership and
selection shape. The exact-dependency gap, current-architecture drift, and
PNC7 replay-proof inconsistency found below are now closed.

## Scope and method

This review compared the [PNC plan](../plans/PureInteractionNetConstruction_2026-09-20.md),
the post-PNC4 contract, the public builtin route, pure builder runner and
selector, managed checkpoint handoff, source fixtures, route-loss fixtures,
the remaining PNC6–PNC7 steps, and current architecture/invariant notes. It
distinguished a test that forces a dependency from one that merely forces a
budget yield. The source-backed inventories were treated as change detectors,
not behavioral proofs.

Focused current-tree verification passed:

```text
cargo test -q --lib public_pure_construction
cargo test -q --lib public_construction_selects_only_the_first_two_results
cargo test -q --lib hidden_builder_reset_shift_resumes_lazy_keys_and_captured_cut
cargo test -q --lib interaction_net_construction_preserves_standard_state_and_control_semantics
cargo test -q --lib interaction_net_construction_backtracks_and_requires_one_result
cargo test -q --lib interaction_net_finalization_reports_invalid_topology
```

The original review did not rerun the broad gates. After its test-only
remediation, `cargo fmt --check`, all-target/all-feature clippy with denied
warnings, `cargo test -q`, and
`scripts/check-interaction-net-profiling.sh` all pass. The first full test run
identified four expected source-inventory count/ledger changes from the new
fixtures; those exact test-only root and access occurrences were reconciled
before the passing rerun.

## Implementation and semantic accounting

The public `InteractionNet` builtin now starts
[`RegionalNetConstruction`](../../src/eval/builtins/net/runner.rs), not
`LazySource::NetConstruction`. A source effect is evaluated to its `eff`
handler, applied to the private builder API and a fresh fixed-width state,
and the resulting ordinary lazy list is observed one front at a time. The
selector retains its first result and current list-front work; it checks for
a second result before decoding the first outcome or its exposed port. Thus
ambiguity takes precedence over malformed exposure, as it did in the legacy
selection order, and a third result is never demanded once ambiguity is known.

The selected exposed port is demanded and brand-checked before the strict
compact netlist is synchronously replayed. A successful replay becomes a
managed WHNF checkpoint in the same value-access transition before the outer
lazy route yields ([handoff](../../src/eval/value.rs)); a later route therefore
does not restart construction just because it took over before final WHNF
publication. There is no independent callback or wait inside replay. This is
a structural one-shot argument, not a measured replay-call count.

Nested `seq`, `alt`, `cut`, `reset`, `shift`, and `fix` operands are recursively
interpreted as effects. A captured shift continuation remains an ordinary
`Value -> Effect` callable; applying it returns an effect for the same runner
to interpret. This matches the agreed `eff` boundary and avoids treating the
continuation itself as an effect. The source-level state/control fixtures and
private reset/shift tests exercise that distinction.

All durable pure-runner state is held beneath the managed builtin checkpoint:
the input state before application, the regional WHNF work, retained first
outcome and list front, or selected state and exposed-port demand. Its visitor
reports the raw value edges of each phase. Construction brands and port tokens
remain edge-free, and the private API has no reflection, heap, task, logger,
environment, or host-I/O member. Public failures receive one
`eval:{op:'net_construction}` frame; private operand-role frames are not
propagated. The old isolated machine and its root-bearing journal remain an
intentional PNC6 deletion target, not a second public implementation.

## Findings

### PNC5R-001 — Public exact-dependency closure is not forced

**Severity:** medium verification gap; no demonstrated result defect.

[PNC5B](../plans/PureInteractionNetConstruction_2026-09-20.md) calls for
exact-dependency, route-loss, and collection schedules in the retained
first-two selector; PNC5D extends that to the public composition. The new
[`public_pure_construction_*` fixtures](../../src/eval/value/tests/w4.rs)
destroy the route after budget-one yields and collect between polls. Their
effect, continuation, selector chunks, and exposed-port values are counted
semantic thunks which complete or fail when demanded. They do not leave an
unresolved `PromisedValue`, assert the exact `WorkDependency::Promise`, or
resume a public construction after assigning that promise. The lower-level
builder path/copy/wire tests do force exact promise waits, but do not cover
the public runner-to-selector-to-exposure handoffs.

Add a small public fixture matrix before declaring this acceptance criterion
closed: block first result-list fragment while a right alternative is ready,
then block the second fragment after the first is retained, and block the
selected exposed port. For those direct waits, assert the exact promise
identity, lose the route, collect, assign the promise, resume, and count the
already-completed prefix. Include one public builder operand wait to cover the
nested runner handoff; that boundary may expose a child-task wait rather than
the child's promise directly. Lower-level builder fixtures remain useful but
are not a substitute for it. A failed exposed-port continuation should still
carry the single public context frame. Explicit barriers or poll boundaries
are required; repeating an uncontrolled test would not prove these schedules.

### PNC5R-002 — Current architecture prose still names the old producer

**Severity:** low documentation drift.

The current [interaction-net invariant note](../agent_context/interaction_nets.md)
still says source construction uses an isolated freer machine and a
branch-local write-only transaction journal, and even says production has
not entered the pure builder. The current [evaluation architecture
note](../architecture/evaluation.md) likewise describes isolated search and
journal replay. [Syntax.md](../Syntax.md) still lists the legacy
`copy_count` automatic context rather than the public `net_construction`
frame. These are present-tense implementation claims, not harmless historical
description. The [source map](../../src/README.md) correctly labels
`construction.rs` legacy, but does not yet name `runner.rs`.

PNC6C already schedules documentation closure, so this does not require a
new implementation phase. Make that checkpoint explicitly cover all four
files, or correct the present-tense architecture notes immediately while
preserving the legacy deletion details for PNC6. In `Syntax.md`, distinguish
the public construction context (which also wraps validation failures) from
the rule for other automatic frames that only decorate forced nested failures.

### PNC5R-003 — PNC7's replay-count demand contradicts PNC5's chosen proof

**Severity:** low plan inconsistency.

[PNC5D](../plans/PureInteractionNetConstruction_2026-09-20.md) explicitly
chooses not to add a production replay probe: replay is synchronous with no
yield or callback boundary, so it calls for memoized net identity plus the
managed terminal handoff. [PNC7](../plans/PureInteractionNetConstruction_2026-09-20.md)
nevertheless says to count replay calls so terminal equality cannot conceal
duplication. Memoized identity verifies repeated *observation* returns the
same net, but it is not a literal replay counter. The code provides the
stronger structural reason: replay returns `Ready` within one builtin poll,
and the owner installs the next managed checkpoint before releasing access.

Rewrite PNC7 to require that structural handoff audit, a forced route-loss
test immediately after the next poll boundary, and memoized identity. The
audit should explicitly include exclusive lazy-claim admission: two routes
must not poll the same construction checkpoint between replay and replacement.
If a future change makes replay interruptible, then add a test-only count or
resumable replay state at that time. Do not add a production probe merely to
satisfy the stale wording.

## Future-phase assessment

PNC6A–C remains a sensible low-risk order: first move the still-shared brand,
port ID, token, and codecs, then delete the obsolete producer route and its
root-heavy journal, then close inventories and docs. The test-only legacy
`ConstructionProbe` lives inside `ConstructionBrand`; PNC6 should remove it
with the legacy tests rather than carry it into the pure identity module.
PNC7 should retain its separate pure construction/checkpoint cycle-reclamation
fixture, since the current legacy `LazySource::NetConstruction` cycle test
does not prove the new graph. No new semantic decision is needed for either
item.

## Resolution — 2026-09-21

**PNC5R-001 — resolved.** Four new public W4 fixtures force first-fragment,
second-fragment, exposed-port, and nested copy-count waits with explicit
budget-one poll boundaries, route loss, and collection. The first-fragment
fixture proves a ready right result is not observed before the blocked left;
the second-fragment fixture counts the retained first prefix. The exposed-port
fixture checks that post-wait validation has exactly one public context frame.
The nested copy-count fixture exposed an important distinction: the public
construction sees an exact wait on a child evaluation task while that child is
blocked on the promise. The fixture now asserts the pending child wait,
assigns the copy count, pumps that same wait to completion, and resumes the
original route. Requiring a direct public `WorkDependency::Promise` there
would have encoded the wrong scheduler boundary. All focused public fixtures
pass, including the existing counted-thunk and source-level parity tests.

**PNC5R-002 — resolved.** The current interaction-net invariant note,
evaluation architecture, syntax context inventory, and source module map now
describe the pure public runner and label `construction.rs` as legacy. PNC6C
retains a final re-audit after deletion, rather than postponing correction of
present-tense claims.

**PNC5R-003 — resolved.** PNC7 now requires a source-claim and same-access
replay-to-WHNF-checkpoint audit, forced route loss at the next poll boundary,
and memoized net identity. It no longer asks for a replay-call counter while
replay remains one synchronous transition.

Next implementation step: PNC6A–C, followed by the separate PNC7 closure
matrix. Re-review if legacy deletion reveals a new ownership seam.
