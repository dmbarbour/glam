# Pure Interaction-Net Construction PNC3 Review — 2026-09-20

Baseline: PNC2 closes at `f857f9dd`. The reviewed PNC3 implementation spans
`fb2b577a` through the fixed-width correction at `c5a3688b`; the general
coordinator repairs `6215e347` and `56d5e804` were discovered while exercising
that implementation.

Status: review complete. No confirmed semantic defect was found in PNC3's
state/control or list-fix implementation. Four focused remediations below gate
PNC4 because the PNC3 completion record currently claims stronger verification
than its direct fixtures provide.

## Scope and Method

This review compared PNC3 with the selected semantics, `docs/Design.md`'s pure
monolith, the legacy reflection handler used as its behavioral oracle, the
post-PNC3 source, and the current PNC4-PNC7 plan. It audited:

- the fixed builder-state, sequence-frame, reset-frame, and outcome schemas;
- return, sequence, alternative, failure, cut, reset, shift, continuation, and
  fixpoint transitions;
- one-future-per-alternative list-fix construction and promise publication;
- exact demand, yield, route-loss, collection, and no-replay evidence;
- managed edges, durable ownership, callbacks, roots, and capability scope;
- the two general scheduler defects exposed while running PNC3; and
- assumptions and checkpoint sizes in PNC4-PNC7.

Source-backed inventories were treated as change detectors, not as behavioral
proof. The review separately inspected what each named fixture actually
forces.

## Outcome

PNC3 implements one ordinary semantic state transformer over ordered lazy
lists. Active sequence/cut frames occupy the fifth field of a fixed-width
builder record. Reset/resume frames remain below the unforgeable `CONTROL_KEY`
inside `user_state`, so whole-state get/set checkpoints retain their intended
control behavior while a currently executing set cannot erase its own
continuation.

All successful state/control paths converge on `dispatch_return`. A cut frame
stops that dispatcher and lets the existing list-effect first-result recipe
select before `builder_continue` resumes the outer stack. Captured
continuations retain their invocation brand, active sequence, and inner reset
frames; invocation restores the caller through a resume frame and rejects a
different construction brand.

Generic list fix now indexes alternatives. Observing alternative `N` creates a
fresh managed promise, reevaluates the fix function with that promise, skips
`N` ordered results through `RegionalListFront`, assigns the selected head,
and exposes a lazy `N + 1` recipe. Exhaustion assigns the established empty
list. This deliberately simple implementation may revisit earlier prefixes,
as the plan records.

No new autonomous producer, reflection task, host callback, task capability,
or runtime root was added. Immediate composition stores ordinary traced values;
demand-capable get/set/reset/shift work lives beneath the existing managed
builtin checkpoint, while fix search remains beneath the existing managed
list-effect checkpoint. The trace implementations cover every raw value held
across a poll.

## Plan-to-Implementation Accounting

| Checkpoint | Reviewed disposition |
| --- | --- |
| PNC3A | Strict sequence/reset codecs and the final exact five-field builder record are present. Structural fields remain ordinary values; tags and control keys are evaluator-owned abstract global paths. |
| PNC3B | Sequence pushes one continuation frame, return owns common dispatch, cut uses a strict delimiter plus canonical `FirstResult`, and alternatives receive the same immutable input state. |
| PNC3C | Reset/shift keys use resumable regional conversion. Shift selects the nearest matching reset, continuation invocation checks construction-brand identity, and cut stages inside captured control are reconstructed. |
| PNC3D | `ListEffectComputation::FixFunction` carries an alternative index; its managed state traces the function, promise, and list-front progress and uses one borrowed step budget. |
| PNC3E | Builder fix hides sequence/reset control, projects the promised outcome's value for the user function, restores control per outcome, and reports strict recursive observation through the ordinary promise cycle path. |
| PNC3F | Behavioral parity and source inventories are broad, but the forced route-loss and several exact semantic latches are incomplete as described in the findings. |

## Ownership and Capability Audit

`DecodedBuilderState` retains raw values only inside a caller-owned value-
access region or the traced `RegionalBuilderBuiltinMachine`. Sequence
continuations are visited explicitly; encoded reset frames remain reachable
through `user_state`. Path/key conversion, dictionary access/update, and WHNF
children all inherit the owning lazy's `source_owner`. No `Root`, public value,
opaque value payload containing semantic edges, erased callback, or nested
access acquisition appears in the builder state.

The hidden builtin family is absent from `import 'std`. Its appearances in the
front-end operator-name table are exhaustive debug/render names for `Builtin`,
not source bindings. Reflection heap, environment, logging, task, shared-heap,
and host-I/O capabilities remain absent.

The raw-value, durable-owner, persistent-edge, recursive-cell, and WHNF
inventories account for the new declarations. The inventories correctly
classify them as regional values or fields of the existing managed builtin and
list-effect checkpoints rather than independent owners.

## Scheduler Corrections Discovered During PNC3

PNC3's deeper causal chains exposed two pre-existing coordinator defects:

1. a client could decide to abandon after sampling quiescence even though its
   exact producer had become claimed; and
2. a dormant or blocked causal producer tail could be mistaken for absence of
   useful progress.

`6215e347` revalidates runtime progress under mutation admission before stable
abandonment. `56d5e804` follows exact dependency chains and recognizes latent
causal progress. Both corrections have forced-order fixtures; neither changes
builder semantics or gives pure construction a scheduler capability.

## Drift Assessment

### Intentional and justified

- **Fixed-width builder state.** The initial PNC3 implementation omitted an
  empty sequence field. `c5a3688b` replaced the four-or-five-field sum with one
  five-field record and made terminal replay require an empty sequence. The
  explicit empty list costs one field but substantially simplifies extension,
  validation, and explanation.
- **Indexed list-fix reevaluation.** Later alternatives reevaluate and skip
  earlier prefixes instead of adding a second persistent search mechanism.
  This is potentially quadratic, but preserves exact semantics and belongs to
  later measured performance work.
- **Protected sequence outside `user_state`.** This is required so `.set []`
  may replace reset state without erasing the sequence executing that set. It
  does not weaken the documented visibility of reset frames through whole-
  state checkpoints.

### No accidental semantic drift found

The reviewed transitions match the pure-monolith oracle for nested resets,
nearest-key shift, state replacement, branch rollback, cut placement,
fixpoint scope hiding, and invocation-local continuations. The findings below
are unclosed proof or representation issues, not observed result mismatches.

## Findings

### PNC3R-001 — Open: builder-specific suspension and route-loss proof is missing

**Severity:** medium verification

PNC3F requires lazy key, continuation, and fix-function suspension at explicit
poll boundaries plus route-loss/collection coverage for the new managed
states. The direct builder tests call the synchronous evaluation facade. They
prove eventual results but do not force the managed builder checkpoint through
yield, dependency publication, route loss, collection, and resumption.

The generic list-fix fixture does force route loss for alternative zero, but
does not cover the builder fix adapters or a later indexed alternative.

Before PNC4, add a retained-source harness which:

- interrupts state and key/path demand on both sides of publication;
- drops and reconstructs the evaluator route, collects, then resumes;
- counts key/function/continuation observations so replay cannot hide behind
  equal values; and
- repeats the exercise while demanding a later builder-fix alternative.

### PNC3R-002 — Open: per-alternative future identity and exhaustion are under-latched

**Severity:** medium verification

`list_effect_fix_allocates_one_future_for_each_observed_alternative` verifies
allocation/publication counts while its function ignores the supplied future.
It therefore does not prove that alternatives receive distinct future
identities or that each future is assigned its own selected head. No direct
fixture demands the exhausted tail and verifies the empty-list assignment.

Add a function whose result wraps its supplied promise without forcing it.
Inspect those nested promise identities under value access for alternatives
zero and one, require them to differ, and verify the exhausted tail publishes
and memoizes the empty result without replay.

### PNC3R-003 — Open: two control-law claims need direct latches

**Severity:** low verification

The continuation fixture uses a captured continuation once in its owning
invocation and once against a foreign invocation. It does not invoke the same
continuation twice within its owner, so the documented non-affine behavior is
not directly proven. Branch-local ordinary state is covered, but reset-stack
isolation across alternatives is not.

Add one same-invocation double-resume fixture and one alternative fixture in
which reset/control changes on a failed branch cannot affect its sibling.

### PNC3R-004 — Open: decoded reset keys use general `Value` equality

**Severity:** low design and auditability

Reset stores a canonical key value, but `BuilderResetFrame::Reset` decodes its
key as an unconstrained `Value`, and shift finds a match with `Value` equality.
Normal source cannot forge the hidden frames, so this is not a demonstrated
source-level defect. It nevertheless weakens malformed-state validation and
adds a new dependency on general `Value` equality while the broader runtime is
moving observations behind explicit access.

Decode the field through `Key::from_value`, store `Key` in the transient frame,
compare keys directly, and re-encode through `key_value`. Add a malformed-key
fixture.

### PNC3R-005 — Resolved: empty sequence changed the builder record width

**Severity:** representation clarity

The optional fifth field made the private state a variable-width sum and made
future extensions ambiguous. `c5a3688b` now emits and requires exactly five
fields, rejects the obsolete four-field shape, and rejects terminal replay
while a sequence remains active.

### PNC3R-006 — Resolved: current documentation and PNC4 ownership drifted

**Severity:** documentation and planning clarity

The plan header still stopped at PNC2, the source map omitted `builder.rs` and
assigned legacy deletion to PNC5 rather than PNC6, and PNC4 combined schema,
operand demand, API assembly, and brand ownership in four bullets. During this
review:

- current ownership docs now include the private pure builder;
- legacy route retirement is assigned consistently to PNC6;
- PNC4 is partitioned into schema, bind/data, copy/wire, and API/closure
  checkpoints; and
- brand allocation and initial-state ownership are assigned to the PNC5
  public runner rather than to individual PNC4 transitions.

## Future-Phase Assessment

PNC4 remains the correct next implementation phase after PNC3R-001 through
PNC3R-004 close. Its regional construction operations should extend the
existing managed builder checkpoint and return through `dispatch_return`;
they should not introduce another machine family. Protected journal fields
must remain O(1)-prepend structures rather than being fully decoded on every
state/control transition.

PNC5 remains coherent after taking explicit ownership of one brand, initial
state, private API application, first-two-outcome selection, exposed-port
demand, and hidden replay. The selector will need its own managed resumable
state and route-loss proof; that detail should be partitioned after PNC4 fixes
the exact API shape.

PNC6 remains the correct point to delete the legacy reflection search,
`LazySource::NetConstruction`, its task-work variant, journals, hosts, and
inventories. PNC5 may make them unreachable in production, but should not mix
behavioral cutover with deletion.

PNC7 remains the whole-pipeline verification and closure gate. Its operation,
selection, and replay counters are still necessary; PNC3's local fixtures do
not replace the public one-shot and cycle-reclamation matrix.

## Verification

The implementation baseline passed immediately before and during this review:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
scripts/check-interaction-net-profiling.sh
```

The ordinary suite reported 1,767 passed library tests and three ignored at
the fixed-width baseline, plus all workspace partitions. The profiling script
passed all thirteen named profiling fixtures on the reviewed tree. Focused
PNC3 tests cover the behavior listed in the completion matrix; findings
PNC3R-001 through PNC3R-003 precisely delimit what those tests do not force.
