# Pure Interaction-Net Construction PNC4 Review — 2026-09-20

Baseline: the compact-journal plan was fixed at `cbe76fb5`. The reviewed PNC4
implementation spans `e77ec006` through `8b92767b`. The later `e6f58772` and
`107d87ec` commits repair unrelated test fixtures which the full PNC4
verification happened to expose; they do not change interaction-net
construction semantics.

Status: review complete; remediation pending. No result defect was demonstrated
in the private PNC4 construction transitions or compact replay. PNC5 should not
cut production over to the pure runner, however, until PNC4R-001 and
PNC4R-002 close the user-visible diagnostic and no-replay proof gaps. The
remaining findings are lower-risk verification and transition-plan cleanup.

## Scope and Method

This review compared PNC4 with the selected semantics, the PNC0 behavioral
baseline, the PNC1 replay boundary, the post-PNC3 representation decisions,
the legacy construction interpreter, the current source, and the PNC5-PNC7
plan. It audited:

- the six-field builder state, compact constructor descriptors, wire pairs,
  reverse-journal insertion, and source-order replay;
- bind, copy, data, and wire arity, operand order, validation, publication,
  return dispatch, and state preservation;
- construction-brand and port-token identity, opacity, and managed-edge shape;
- lazy payload handling, exact dependencies, route loss, collection, and
  no-replay claims;
- the private API's exact capability surface;
- callback, scheduler, root, mutator, durable-owner, and persistent-edge
  boundaries; and
- assumptions which PNC5 and PNC6 make about the now-concrete PNC4 types.

Source-backed inventories were treated as change detectors rather than as
behavioral proofs. Each completion claim was also compared with the ordering
which its named fixture actually forces.

## Outcome

The implementation has the intended architectural shape. Builder state is one
fixed six-field strict record. Constructor and wire journals are independent
persistent list spines, and construction transitions prepend without walking
their retained prefixes. Bind and data demand only the state; data retains its
payload as an ordinary unforced edge. Copy evaluates its count before state,
while wire evaluates left port, right port, then state. Successful operations
all re-enter the PNC3 return dispatcher.

Replay is one synchronous, callback-free value-access region. It traverses
constructors in source order, derives their logical IDs, checks the derived
cursor against `next_port`, then applies source-ordered wire pairs and asks
`NetBuilder` to validate the final topology. The legacy construction machine
adapts its selected journal to the same compact protocol, so there is only one
topology-validation and lowering path.

Construction brands and ports remain edge-free opaque tokens. The builder
machine's demand state, evaluated operands, decoded state, and nested regional
work are all traced beneath the existing managed builtin checkpoint. No new
producer route, runtime root, reflection task, host callback, shared heap, or
external capability was introduced.

Production still uses `NetConstructionMachine`; the PNC4 initial state and
private API are intentionally private and otherwise unused outside focused
tests. Findings below therefore do not describe a current source-visible
regression. They identify what must be closed before PNC5 makes this path
authoritative.

## Findings

### PNC4R-001 — Public construction needs one selected diagnostic boundary

**Severity:** medium integration blocker

The legacy interpreter enriches a failed copy-count demand with
`eval:{op:'copy_count}` and enriches failed construction search with
`eval:{op:'net_construction}`. The first frame is not part of a coherent
operation-local policy. It originated as the required context-label argument
to the former generic numeric-index helper and was copied forward when copy
evaluation became resumable. Equivalent demands for left and right wire
ports never gained frames, nor do the pure builder's reset/shift keys,
get/set paths, builder state, or exposed port. No focused test asserts the
`copy_count` frame.

The pure builder consistently propagates failures from all of those regional
demands unchanged. That is the preferable local contract. Adding a distinct
frame for every private operand role would expose implementation staging,
produce noisy stacks, and still leave an arbitrary question about which
roles deserve labels. `net_construction`, by contrast, is the stable public
answer to why any of these values was demanded.

The pure builder's immediate copy-validation text also differs from the
legacy public text. None of this is observable through production today, but
a direct PNC5 cutover would silently change some message text unless the
intended public behavior is latched deliberately.

Before cutover:

1. latch the intended pure failure for failed copy-count, wire-port, and
   reset/shift-key demands, plus a general failing construction program;
2. keep private builder demands transparent: do not reproduce the legacy
   `copy_count` frame and do not add parallel `wire_port`, `reset_key`, path,
   state, or exposed-port frames;
3. make the PNC5 public runner add `net_construction` exactly once around
   failures from effect application, builder execution, selection, exposed
   port demand, and replay; and
4. decide whether immediate validation strings are compatibility promises or
   may be updated together with the baseline fixtures.

The outer context belongs naturally to PNC5's public composition. PNC6 should
remove `copy_count` with the legacy interpreter and update `docs/Syntax.md`,
whose current-operation inventory accurately describes the transitional
production path. If operand-role contexts later prove useful, design them as
one semantic policy with structured operation and operand fields rather than
preserving this one historical label.

### PNC4R-002 — Operand route-loss evidence can conceal replay

**Severity:** medium verification

`builder_wire_checkpoint_preserves_left_to_right_operand_and_state_dependencies`
forces the left, right, and state promise dependencies in order, destroys the
route, collects, and eventually observes one wire record. That proves exact
dependency publication and retained terminal state. It does not prove that a
managed builtin checkpoint remains installed across collection before another
poll has an opportunity to recreate it. Restarting from an earlier phase can
consume an already resolved promise immediately and still produce the same
dependency sequence and one final journal entry.

This falls short of PNC4C's explicit rule that terminal equality is not enough
to prove absence of replay. The same matrix has no forced route-loss case for
the copy-count operand, and no builder-specific failure fixture at the copy,
left-wire, right-wire, or state boundary.

Close this entirely in `src/eval/value/tests/w4.rs`; it requires no production
probe or reflection task:

1. strengthen the builtin route-loss helper to assert that the managed
   `Builtin` checkpoint exists both before route destruction and immediately
   after collection and route reconstruction, before the resumed machine is
   polled;
2. retain the wire fixture's exact left, right, then state promise-ID sequence
   and single wire-journal entry;
3. add the corresponding copy fixture, forcing count then state dependencies
   across route loss and observing exactly one constructor-journal entry; and
4. add failure-order fixtures in which copy-count, left-wire, right-wire, and
   state demands fail in turn, while counted later operands prove that no
   operation beyond the failure boundary was observed.

Counted `LazyValue::semantic_thunk` shells are useful for the failure-order
matrix and for proving that underlying operand production is not repeated.
They do not count later reads of an already memoized value, so they must not be
presented as independent proof that every cache lookup occurred once. That
stronger property is neither semantic replay nor required here: the contract
is retained resumable state, no repeated semantic computation, and no duplicate
journal publication. If avoiding even cached re-observation later becomes a
performance requirement, it needs explicit test-only builder-phase
instrumentation. Uncontrolled repetition is not evidence for any of these
schedules.

### PNC4R-003 — Constructor source order is asserted by code, not behavior

**Severity:** medium verification

Both mixed replay fixtures only assert that replay returns `Value::Net`. Their
bind/data/copy topology can remain a valid closed net under more than one
constructor-to-logical-ID assignment, so that terminal shape does not prove
the documented source-order mapping. The implementation correctly iterates
the reverse journal backwards, but the authoritative ordering contract lacks
a regression which would fail if that direction changed.

Add one compact replay fixture which distinguishes the assigned logical IDs,
either through a read-only topology assertion or through an evaluated result
whose value changes if bind/data/copy constructor order is reversed. Retain a
separate topology-success fixture; successful finalization alone should not be
the order oracle.

### PNC4R-004 — Closure claims have several low-cost missing latches

**Severity:** low verification

The exact-API fixture proves only the key set. It would still pass if two
members mapped to the wrong hidden builtins or carried the wrong arities. The
fixpoint preservation fixture starts with one constructor record but no wire
record, so it does not directly prove that both protected journals cross the
fix adapter. The malformed compact-record matrix covers the principal shapes
but not an unknown constructor tag or malformed wire field/count.

Tighten these fixtures by:

- asserting the complete `name -> Builtin -> arity` table;
- entering fix with one constructor and one wire, then observing `(1, 1)`;
  and
- adding an unknown constructor tag plus malformed wire arity and endpoint
  type cases.

These are test-only changes and require no semantic redesign.

### PNC4R-005 — Phase descriptions lag the concrete ownership seams

**Severity:** low documentation and planning clarity

The plan header still called PNC4 the next phase after marking it complete,
and PNC4's exit text claimed that a complete construction program runs even
though the program-to-private-API runner is deliberately PNC5 work. Those two
local statements are corrected with this review.

Two future seams also need explicit checkpoints:

- PNC5 now has enough concrete structure to partition into runner assembly,
  first-two-result selection, public lazy composition/cutover, and forced
  route-loss plus diagnostic-parity closure. The selector needs its own
  retained regional state; it should not be hidden inside a single large
  `RegionalNetMachine` edit.
- PNC6 cannot simply delete `construction.rs`. `ConstructionBrand`,
  `ConstructionPortId`, the edge-free opaque token family, and their codecs
  are shared by the pure builder and netlist replay. Move that protocol to a
  small pure-construction identity module before deleting the legacy journal,
  effect specialization, and machine.

The review leaves those detailed plan edits until the findings are accepted,
so checkpoint boundaries can be revised together rather than piecemeal.

## No Accidental Semantic Drift Found

- The compact schema contains no derived allocation-port fields.
- `next_port` remains a checked summary rather than replay's source of truth.
- Wire operands are demanded left-to-right and wire records retain only IDs.
- `.data` payloads are neither demanded by the builder nor by replay.
- Failed alternatives share but cannot mutate either journal prefix.
- User state cannot address protected journals or the active sequence stack.
- Reset state remains below the hidden key inside `user_state`.
- The private API contains no reflection, heap, task, logger, environment,
  exit, or host-I/O capability.
- The hidden replay module retains its source-backed callback/scheduler/root
  exclusion latch.

## Future-Phase Assessment

PNC5 remains the correct next behavioral phase after PNC4R-001 through
PNC4R-004. Its first checkpoint should compose an effect function with the
exact PNC4 API and one runner-owned brand/state without changing production.
A second checkpoint should implement and force-test a resumable first-two
selector. A third should compose exposed-port validation and replay beneath
the public construction lazy, preserve the outer diagnostic context, and cut
production over. A final checkpoint should unignore the legacy route-loss
regression in its pure replacement form and prove one operation, selection,
and replay.

PNC6 should first relocate the identity/token protocol, then remove the
legacy producer route and root-bearing journal, and finally reconcile its
inventories. PNC7's whole-pipeline cycle, backedge, capability, and
aggressive-collection matrix remains necessary; local PNC4 fixtures do not
replace it.

## Verification Baseline

The reviewed PNC4 implementation passed before this documentation-only review:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
scripts/check-interaction-net-profiling.sh
```

The ordinary suite reported 1,778 passed library tests and three ignored,
plus every workspace partition. The profiling script passed all thirteen
named profiling fixtures. This review does not treat those broad passes as
evidence for the specific missing schedules above.
